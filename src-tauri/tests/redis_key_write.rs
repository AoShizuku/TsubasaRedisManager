//! 新建/克隆的真实 Redis 回归；默认忽略，由 scripts/dev/e2e.ps1 运行。
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use serial_test::serial;
use tsubasa_redis_manager_lib::error::{AppError, AppResult};
use tsubasa_redis_manager_lib::models::{
    ClonePhase, CloneTaskSnapshot, ConnectionConfig, RedisValue,
};
use tsubasa_redis_manager_lib::services::connection::{
    close_connection, execute_cli, open_connection,
};
use tsubasa_redis_manager_lib::services::task::cancel_task;
use tsubasa_redis_manager_lib::services::values::{
    clone_key, create_key, delete_keys, get_clone_task, key_exists, set_string_value,
    start_clone_task, CreateKeyValue, KeyExpiry,
};
use tsubasa_redis_manager_lib::state::AppState;

fn auth_config() -> ConnectionConfig {
    let mut value = config(false, 0, false);
    value.port = std::env::var("TEST_REDIS_AUTH_PORT")
        .unwrap()
        .parse()
        .unwrap();
    value.auth = Some("correct-horse".into());
    value
}

async fn settled_clone(state: &AppState, id: &str) -> CloneTaskSnapshot {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let detail = get_clone_task(state, id).await.unwrap();
            if matches!(
                detail.phase,
                ClonePhase::Completed
                    | ClonePhase::Cancelled
                    | ClonePhase::Failed
                    | ClonePhase::Uncertain
            ) {
                return detail;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("clone should reach a terminal state")
}

fn config(cluster: bool, db: i64, readonly: bool) -> ConnectionConfig {
    ConnectionConfig {
        host: std::env::var("TEST_REDIS_HOST").expect("TEST_REDIS_HOST is required"),
        port: std::env::var(if cluster {
            "PHASE2_CLUSTER_PORT"
        } else {
            "TEST_REDIS_PORT"
        })
        .expect("test Redis port is required")
        .parse()
        .unwrap(),
        auth: None,
        username: None,
        db: Some(db),
        name: Some("key-write e2e".into()),
        separator: None,
        connection_read_only: Some(readonly),
        cluster: Some(cluster),
        ssl_options: None,
        ssh_options: None,
        sentinel_options: None,
    }
}

fn initial(value: Value) -> CreateKeyValue {
    serde_json::from_value(value).unwrap()
}
fn string(bytes: &[u8]) -> CreateKeyValue {
    initial(json!({"type": "string", "valueBase64": STANDARD.encode(bytes)}))
}
fn deadline() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + 600_123
}
async fn cli(state: &AppState, id: &str, command: &str) -> AppResult<Value> {
    Ok(serde_json::to_value(execute_cli(state, id, command).await?.value).unwrap())
}
async fn integer(state: &AppState, id: &str, command: &str) -> AppResult<i64> {
    match execute_cli(state, id, command).await?.value {
        RedisValue::Integer(number) => Ok(number),
        value => panic!("expected integer for {command}: {value:?}"),
    }
}

async fn six_types(cluster: bool) {
    let state = AppState::default();
    let source = open_connection(&state, config(cluster, 0, false))
        .await
        .unwrap()
        .connection_id;
    let target = open_connection(&state, config(cluster, if cluster { 0 } else { 1 }, false))
        .await
        .unwrap()
        .connection_id;
    let token = uuid::Uuid::new_v4();
    let mut source_keys = Vec::new();
    let mut target_keys = Vec::new();
    let expiry = deadline();
    let binary = STANDARD.encode(b"\0\xff\x80\r\n");
    let fields = json!([{"fieldBase64": binary, "valueBase64": STANDARD.encode("中文\n ")}, {"fieldBase64": "", "valueBase64": ""}]);
    let fixtures = [
        json!({"type": "string", "valueBase64": binary}),
        json!({"type": "hash", "fields": fields}),
        json!({"type": "list", "elementsBase64": [binary, "", binary]}),
        json!({"type": "set", "membersBase64": [binary, ""]}),
        json!({"type": "zset", "members": [{"valueBase64": binary, "score": "-inf"}, {"valueBase64": "", "score": "1.25"}]}),
        json!({"type": "stream", "fields": fields}),
    ];
    let result = async {
        for fixture in fixtures {
            let kind = fixture["type"].as_str().unwrap().to_string();
            let key = format!("write:{token}:source:{kind}");
            // 同 DB 用新名，跨 DB 使用完全相同的名字。
            let copied = if cluster {
                format!("write:{token}:target:{kind}")
            } else {
                key.clone()
            };
            let local = format!("write:{token}:local:{kind}");
            source_keys.extend([key.clone(), local.clone()]);
            target_keys.push(copied.clone());
            let created = create_key(
                &state,
                &source,
                &key,
                initial(fixture),
                KeyExpiry::Absolute {
                    milliseconds: expiry,
                },
            )
            .await?;
            assert_eq!(created.key_type, kind);
            assert_eq!(
                integer(&state, &source, &format!("PEXPIRETIME {key}")).await?,
                expiry
            );
            if kind == "stream" {
                cli(&state, &source, &format!("XGROUP CREATE {key} group 0")).await?;
                cli(
                    &state,
                    &source,
                    &format!("XREADGROUP GROUP group consumer COUNT 1 STREAMS {key} >"),
                )
                .await?;
            }
            let original = cli(&state, &source, &format!("DUMP {key}")).await?;
            clone_key(&state, &source, &target, &key, &copied, KeyExpiry::Preserve).await?;
            assert_eq!(
                cli(&state, &target, &format!("DUMP {copied}")).await?,
                original,
                "{kind} must preserve all serialized contents"
            );
            assert_eq!(
                integer(&state, &target, &format!("PEXPIRETIME {copied}")).await?,
                expiry
            );
            if kind == "stream" {
                assert_eq!(
                    cli(&state, &source, &format!("XPENDING {key} group")).await?,
                    cli(&state, &target, &format!("XPENDING {copied} group")).await?
                );
            }
            // 同一个 runtime 既作源又作目标：验证无死锁，以及自定义不过期。
            clone_key(&state, &source, &source, &key, &local, KeyExpiry::Persist).await?;
            assert_eq!(
                integer(&state, &source, &format!("PTTL {local}")).await?,
                -1
            );
            let collision = clone_key(&state, &source, &target, &key, &copied, KeyExpiry::Persist)
                .await
                .unwrap_err();
            assert_eq!(collision.payload().code, "redis.key_exists");
            let collision = create_key(
                &state,
                &source,
                &key,
                string(b"must not overwrite"),
                KeyExpiry::Persist,
            )
            .await
            .unwrap_err();
            assert_eq!(collision.payload().code, "redis.key_exists");
            assert_eq!(
                cli(&state, &source, &format!("DUMP {key}")).await?,
                original
            );
            assert_eq!(
                integer(&state, &target, &format!("PEXPIRETIME {copied}")).await?,
                expiry
            );
            if cluster {
                assert_ne!(
                    integer(&state, &source, &format!("CLUSTER KEYSLOT {key}")).await?,
                    integer(&state, &target, &format!("CLUSTER KEYSLOT {copied}")).await?
                );
            }
        }
        Ok::<_, AppError>(())
    }
    .await;
    delete_keys(&state, &source, &source_keys).await.unwrap();
    delete_keys(&state, &target, &target_keys).await.unwrap();
    close_connection(&state, &source).await.unwrap();
    close_connection(&state, &target).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[serial]
#[ignore = "requires local Redis 7+"]
async fn six_types_create_and_clone_across_databases_with_exact_expiry() {
    six_types(false).await
}

#[tokio::test]
#[serial]
#[ignore = "requires local Redis cluster"]
async fn six_types_create_and_clone_across_cluster_slots() {
    six_types(true).await
}

#[tokio::test]
#[serial]
#[ignore = "requires local Redis 7+"]
async fn creation_races_and_validation_do_not_overwrite_or_leave_partial_keys() {
    let state = AppState::default();
    let a = open_connection(&state, config(false, 0, false))
        .await
        .unwrap()
        .connection_id;
    let b = open_connection(&state, config(false, 0, false))
        .await
        .unwrap()
        .connection_id;
    let key = format!("write:race:{}", uuid::Uuid::new_v4());
    let invalid = format!("{key}:invalid");
    let result = async {
        let (first, second) = tokio::join!(
            create_key(&state, &a, &key, string(b"first"), KeyExpiry::Relative { milliseconds: 60_123 }),
            create_key(&state, &b, &key, string(b"second"), KeyExpiry::Relative { milliseconds: 60_123 })
        );
        assert_ne!(first.is_ok(), second.is_ok());
        assert_eq!(first.err().or_else(|| second.err()).unwrap().payload().code, "redis.key_exists");
        let ttl = integer(&state, &a, &format!("PTTL {key}")).await?;
        assert!((1..=60_123).contains(&ttl));
        for value in [
            json!({"type": "zset", "members": [{"score": "1", "valueBase64": "YQ=="}, {"score": "NaN", "valueBase64": "Yg=="}]}),
            json!({"type": "list", "elementsBase64": ["YQ==", "!invalid"]}),
            json!({"type": "hash", "fields": []}),
            json!({"type": "hash", "fields": [{"fieldBase64": "YQ==", "valueBase64": ""}, {"fieldBase64": "YQ==", "valueBase64": ""}]}),
        ] {
            assert!(create_key(&state, &a, &invalid, initial(value), KeyExpiry::Persist).await.is_err());
            assert_eq!(integer(&state, &a, &format!("EXISTS {invalid}")).await?, 0);
        }
        let error = create_key(&state, &a, &invalid, string(b""), KeyExpiry::Absolute { milliseconds: 1 }).await.unwrap_err();
        assert_eq!(error.payload().code, "redis.key_invalid_expiry");
        assert_eq!(integer(&state, &a, &format!("EXISTS {invalid}")).await?, 0);
        let error = clone_key(&state, &a, &b, &key, &key, KeyExpiry::Preserve).await.unwrap_err();
        assert_eq!(error.payload().code, "redis.key_invalid_target");
        let error = clone_key(&state, &a, &b, &invalid, &format!("{key}:missing"), KeyExpiry::Preserve).await.unwrap_err();
        assert_eq!(error.payload().code, "redis.key_source_missing");
        Ok::<_, AppError>(())
    }.await;
    delete_keys(&state, &a, &[key, invalid]).await.unwrap();
    close_connection(&state, &a).await.unwrap();
    close_connection(&state, &b).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[serial]
#[ignore = "requires local Redis 7+"]
async fn clone_reads_readonly_sources_and_preserves_full_binary_values() {
    let state = AppState::default();
    let writer = open_connection(&state, config(false, 0, false))
        .await
        .unwrap()
        .connection_id;
    let reader = open_connection(&state, config(false, 0, true))
        .await
        .unwrap()
        .connection_id;
    let key = format!("write:binary:{}", uuid::Uuid::new_v4());
    let copied = format!("{key}:copy");
    let result = async {
        create_key(
            &state,
            &writer,
            &key,
            string(&vec![0xff; 300 * 1024]),
            KeyExpiry::Persist,
        )
        .await?;
        clone_key(&state, &reader, &writer, &key, &copied, KeyExpiry::Preserve).await?;
        assert_eq!(
            integer(&state, &writer, &format!("STRLEN {copied}")).await?,
            300 * 1024
        );
        assert_eq!(
            cli(&state, &writer, &format!("DUMP {key}")).await?,
            cli(&state, &writer, &format!("DUMP {copied}")).await?
        );
        assert_eq!(
            integer(&state, &writer, &format!("PTTL {copied}")).await?,
            -1
        );
        let error = clone_key(
            &state,
            &writer,
            &reader,
            &key,
            &format!("{key}:blocked"),
            KeyExpiry::Preserve,
        )
        .await
        .unwrap_err();
        assert_eq!(error.payload().code, "redis.readonly_write_rejected");
        assert!(create_key(
            &state,
            &reader,
            &format!("{key}:blocked"),
            string(b""),
            KeyExpiry::Persist
        )
        .await
        .is_err());
        Ok::<_, AppError>(())
    }
    .await;
    delete_keys(&state, &writer, &[key, copied]).await.unwrap();
    close_connection(&state, &writer).await.unwrap();
    close_connection(&state, &reader).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[serial]
#[ignore = "requires local Redis with ACL administration"]
async fn denied_ttl_does_not_leave_a_permanent_key_even_when_del_is_denied() {
    let state = AppState::default();
    let admin = open_connection(&state, config(false, 0, false))
        .await
        .unwrap()
        .connection_id;
    let user = format!("write-acl-{}", uuid::Uuid::new_v4());
    let key = format!("write:{user}");
    cli(
        &state,
        &admin,
        &format!("ACL SETUSER {user} on >test-password ~write:* +@all -pexpireat -del"),
    )
    .await
    .unwrap();
    let mut restricted = config(false, 0, false);
    restricted.username = Some(user.clone());
    restricted.auth = Some("test-password".into());
    let restricted = open_connection(&state, restricted)
        .await
        .unwrap()
        .connection_id;
    let result = async {
        let value = initial(
            json!({"type": "hash", "fields": [{"fieldBase64": "YQ==", "valueBase64": "Yg=="}]}),
        );
        assert!(create_key(
            &state,
            &restricted,
            &key,
            value,
            KeyExpiry::Relative {
                milliseconds: 100_000
            }
        )
        .await
        .is_err());
        assert_eq!(integer(&state, &admin, &format!("EXISTS {key}")).await?, 0);
        assert!(!key_exists(&state, &restricted, &key).await?);
        cli(&state, &admin, &format!("ACL SETUSER {user} -exists")).await?;
        assert!(
            key_exists(&state, &restricted, &key).await.is_err(),
            "denied EXISTS must not report a missing key"
        );
        Ok::<_, AppError>(())
    }
    .await;
    close_connection(&state, &restricted).await.unwrap();
    cli(&state, &admin, &format!("ACL DELUSER {user}"))
        .await
        .unwrap();
    delete_keys(&state, &admin, &[key]).await.unwrap();
    close_connection(&state, &admin).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[serial]
#[ignore = "requires standalone, authenticated Redis and cluster"]
async fn cross_connection_clones_preserve_six_types_including_cluster_routes() {
    for (source_cluster, target_cluster) in [(false, false), (false, true), (true, false)] {
        let state = AppState::default();
        let source = open_connection(&state, config(source_cluster, 0, false))
            .await
            .unwrap()
            .connection_id;
        let target = open_connection(
            &state,
            if target_cluster {
                config(true, 0, false)
            } else {
                auth_config()
            },
        )
        .await
        .unwrap()
        .connection_id;
        let token = uuid::Uuid::new_v4();
        let mut keys = Vec::new();
        let result = async {
            for (kind, seed) in [
                ("string", "SET {key} value"),
                ("hash", "HSET {key} a b c d"),
                ("list", "RPUSH {key} a b a"),
                ("set", "SADD {key} a b"),
                ("zset", "ZADD {key} -inf a 1.25 b"),
                ("stream", "XADD {key} * a b"),
            ] {
                let key = format!("write:cross:{token}:{kind}");
                keys.push(key.clone());
                cli(&state, &source, &seed.replace("{key}", &key)).await?;
                cli(&state, &source, &format!("PEXPIRE {key} 600123")).await?;
                let started = start_clone_task(
                    None,
                    &state,
                    &source,
                    &target,
                    &key,
                    &key,
                    KeyExpiry::Preserve,
                )
                .await?;
                let detail = settled_clone(&state, &started.task_id).await;
                assert_eq!(detail.phase, ClonePhase::Completed, "{}", detail.message);
                assert_eq!(detail.key_type.as_deref(), Some(kind));
                assert!(
                    cli(&state, &source, &format!("DUMP {key}")).await?
                        == cli(&state, &target, &format!("DUMP {key}")).await?,
                    "native snapshot must round trip"
                );
                let source_deadline =
                    integer(&state, &source, &format!("PEXPIRETIME {key}")).await?;
                let target_deadline =
                    integer(&state, &target, &format!("PEXPIRETIME {key}")).await?;
                assert!((source_deadline - 2000..=source_deadline + 1).contains(&target_deadline));
            }
            Ok::<_, AppError>(())
        }
        .await;
        delete_keys(&state, &source, &keys).await.unwrap();
        delete_keys(&state, &target, &keys).await.unwrap();
        close_connection(&state, &source).await.unwrap();
        close_connection(&state, &target).await.unwrap();
        result.unwrap();
    }
}

#[tokio::test]
#[serial]
#[ignore = "requires standalone and authenticated Redis"]
async fn background_clone_transfers_twenty_megabytes_and_cancels_before_writing() {
    let state = AppState::default();
    let source = open_connection(&state, config(false, 0, false))
        .await
        .unwrap()
        .connection_id;
    let target = open_connection(&state, auth_config())
        .await
        .unwrap()
        .connection_id;
    let key = format!("write:large:{}", uuid::Uuid::new_v4());
    let cancelled_key = format!("{key}:cancelled");
    let result = async {
        let mut seed = 123456789u32;
        let payload: Vec<u8> = (0..20 * 1024 * 1024)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        set_string_value(&state, &source, &key, &STANDARD.encode(&payload)).await?;
        let started = start_clone_task(
            None,
            &state,
            &source,
            &target,
            &key,
            &key,
            KeyExpiry::Persist,
        )
        .await?;
        let detail = settled_clone(&state, &started.task_id).await;
        assert_eq!(detail.phase, ClonePhase::Completed, "{}", detail.message);
        assert!(detail.bytes.unwrap() > 16 * 1024 * 1024);
        assert_eq!(
            integer(&state, &target, &format!("STRLEN {key}")).await?,
            payload.len() as i64
        );
        assert!(
            cli(&state, &source, &format!("DUMP {key}")).await?
                == cli(&state, &target, &format!("DUMP {key}")).await?
        );
        cli(&state, &source, "CLIENT PAUSE 500 ALL").await?;
        let started = start_clone_task(
            None,
            &state,
            &source,
            &target,
            &key,
            &cancelled_key,
            KeyExpiry::Persist,
        )
        .await?;
        cancel_task(&state, &started.task_id).await?;
        let detail = settled_clone(&state, &started.task_id).await;
        assert_eq!(detail.phase, ClonePhase::Cancelled, "{}", detail.message);
        assert!(!key_exists(&state, &target, &cancelled_key).await?);
        Ok::<_, AppError>(())
    }
    .await;
    delete_keys(&state, &source, std::slice::from_ref(&key))
        .await
        .unwrap();
    delete_keys(&state, &target, &[key, cancelled_key])
        .await
        .unwrap();
    close_connection(&state, &source).await.unwrap();
    close_connection(&state, &target).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[serial]
#[ignore = "requires Redis 6 and Redis 7 instances"]
async fn rdb_version_incompatibility_is_explicit_and_older_snapshots_restore_forward() {
    let state = AppState::default();
    let modern = open_connection(&state, config(false, 0, false))
        .await
        .unwrap()
        .connection_id;
    let mut legacy_config = config(false, 0, false);
    legacy_config.port = std::env::var("TEST_REDIS_LEGACY_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let legacy = open_connection(&state, legacy_config)
        .await
        .unwrap()
        .connection_id;
    let key = format!("write:version:{}", uuid::Uuid::new_v4());
    let backwards = format!("{key}:old");
    let forward = format!("{key}:new");
    let result = async {
        create_key(&state, &modern, &key, string(b"modern"), KeyExpiry::Persist).await?;
        let started = start_clone_task(
            None,
            &state,
            &modern,
            &legacy,
            &key,
            &backwards,
            KeyExpiry::Preserve,
        )
        .await?;
        let detail = settled_clone(&state, &started.task_id).await;
        assert_eq!(detail.phase, ClonePhase::Failed);
        assert_eq!(detail.error_code.as_deref(), Some("redis.key_incompatible"));
        assert!(!key_exists(&state, &legacy, &backwards).await?);
        create_key(
            &state,
            &legacy,
            &backwards,
            string(b"legacy"),
            KeyExpiry::Relative {
                milliseconds: 600123,
            },
        )
        .await?;
        clone_key(
            &state,
            &legacy,
            &modern,
            &backwards,
            &forward,
            KeyExpiry::Preserve,
        )
        .await?;
        assert_eq!(
            cli(&state, &modern, &format!("GET {forward}")).await?,
            cli(&state, &legacy, &format!("GET {backwards}")).await?
        );
        assert!(integer(&state, &modern, &format!("PTTL {forward}")).await? > 0);
        Ok::<_, AppError>(())
    }
    .await;
    delete_keys(&state, &modern, &[key, forward]).await.unwrap();
    delete_keys(&state, &legacy, &[backwards]).await.unwrap();
    close_connection(&state, &modern).await.unwrap();
    close_connection(&state, &legacy).await.unwrap();
    result.unwrap();
}
