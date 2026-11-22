//! 服务端身份与能力探测的端到端矩阵。默认忽略，由 scripts/dev/e2e.ps1 运行。
//!
//! 身份字段与能力结论只能对着真实服务端定：Valkey 的 `redis_version` 是兼容声明、
//! KeyDB 只靠一行 `features` 认出来、Dragonfly 的版本号带 `df-v` 前缀 —— 这些都是在
//! 这里实测后才写进 `services/capabilities.rs` 的。见 doc/backend/capabilities.md。

use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serial_test::serial;
use tsubasa_redis_manager_lib::error::AppError;
use tsubasa_redis_manager_lib::models::{
    ConnectionConfig, ConnectionOpened, RedisValue, TtlCondition, TtlMode,
};
use tsubasa_redis_manager_lib::services::capabilities::{
    Capability, CapabilityChangeEvent, CapabilityId, CapabilityState, ServerFlavor,
};
use tsubasa_redis_manager_lib::services::connection::{
    close_connection, execute_cli, open_connection,
};
use tsubasa_redis_manager_lib::services::task::{apply_ttl_for_connection, TtlSpec};
use tsubasa_redis_manager_lib::services::values::{
    create_key, set_key_ttl, set_string_value, CreateKeyValue, KeyExpiry,
};
use tsubasa_redis_manager_lib::state::AppState;

fn port(var: &str) -> u16 {
    std::env::var(var)
        .unwrap_or_else(|_| panic!("{var} is required (run scripts/dev/e2e.ps1)"))
        .parse()
        .unwrap()
}

fn config(port_var: &str) -> ConnectionConfig {
    ConnectionConfig {
        host: std::env::var("TEST_REDIS_HOST").expect("TEST_REDIS_HOST is required"),
        port: port(port_var),
        auth: None,
        username: None,
        db: None,
        name: Some(format!("capabilities e2e ({port_var})")),
        separator: None,
        connection_read_only: Some(false),
        cluster: None,
        ssl_options: None,
        ssh_options: None,
        sentinel_options: None,
    }
}

fn state_of(opened: &ConnectionOpened, id: CapabilityId) -> CapabilityState {
    opened.capabilities[&id].state
}

async fn open(state: &AppState, port_var: &str) -> ConnectionOpened {
    open_connection(state, config(port_var))
        .await
        .unwrap_or_else(|err| panic!("{port_var} should open: {err:?}"))
}

async fn cli(state: &AppState, id: &str, line: &str) -> RedisValue {
    execute_cli(state, id, line)
        .await
        .unwrap_or_else(|err| panic!("{line} failed: {err:?}"))
        .value
}

async fn get(state: &AppState, id: &str, key: &str) -> String {
    match cli(state, id, &format!("GET {key}")).await {
        RedisValue::String(value) => value,
        other => panic!("GET returned {other:?}"),
    }
}

async fn pttl(state: &AppState, id: &str, key: &str) -> i64 {
    match cli(state, id, &format!("PTTL {key}")).await {
        RedisValue::Integer(value) => value,
        other => panic!("PTTL returned {other:?}"),
    }
}

/// 保存后 TTL 还在、值已更新 —— STRING 保存的验收口径，三条路径共用。
async fn assert_save_keeps_ttl(state: &AppState, id: &str) {
    let key = format!("cap:keepttl:{}", uuid::Uuid::new_v4());
    cli(state, id, &format!("SET {key} before EX 600")).await;
    let saved = set_string_value(state, id, &key, &STANDARD.encode(b"after")).await;
    let ttl = pttl(state, id, &key).await;
    let value = get(state, id, &key).await;
    cli(state, id, &format!("DEL {key}")).await;

    saved.unwrap_or_else(|err| panic!("save should succeed: {err:?}"));
    assert!(ttl > 590_000, "TTL should survive the save, got {ttl}");
    assert_eq!(value, "after");
}

fn recording_sink(state: &AppState) -> Arc<Mutex<Vec<CapabilityChangeEvent>>> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let _ = state.capability_sink.set(Arc::new(move |event| {
        sink.lock().unwrap().push(event);
    }));
    events
}

async fn force_capability(state: &AppState, id: &str, which: CapabilityId, to: CapabilityState) {
    let sessions = state.sessions.lock().await;
    sessions.get(id).unwrap().runtime.set_capability(
        which,
        Capability {
            state: to,
            usable: true,
            reason: "forced by test".into(),
        },
    );
}

/* ── 身份 ─────────────────────────────────────────────── */

#[tokio::test]
#[serial]
#[ignore = "requires the e2e.ps1 server matrix"]
async fn redis_7_baseline_is_fully_supported() {
    let state = AppState::default();
    let opened = open(&state, "TEST_REDIS_PORT").await;

    assert_eq!(opened.server.flavor, ServerFlavor::Redis);
    assert_eq!(opened.server.version, opened.server.compat_version);
    for (id, capability) in &opened.capabilities {
        assert_eq!(
            capability.state,
            CapabilityState::Supported,
            "{id:?}: {}",
            capability.reason
        );
    }
    close_connection(&state, &opened.connection_id)
        .await
        .unwrap();
}

#[tokio::test]
#[serial]
#[ignore = "requires the e2e.ps1 server matrix"]
async fn redis_cluster_probes_one_node_and_is_fully_supported() {
    let state = AppState::default();
    let mut cluster = config("PHASE2_CLUSTER_PORT");
    cluster.cluster = Some(true);
    let opened = open_connection(&state, cluster).await.unwrap();

    assert_eq!(opened.server.flavor, ServerFlavor::Redis);
    for (id, capability) in &opened.capabilities {
        assert_eq!(
            capability.state,
            CapabilityState::Supported,
            "{id:?}: {}",
            capability.reason
        );
    }
    close_connection(&state, &opened.connection_id)
        .await
        .unwrap();
}

#[tokio::test]
#[serial]
#[ignore = "requires the e2e.ps1 server matrix"]
async fn valkey_is_judged_by_its_own_version() {
    for (port_var, major) in [("TEST_VALKEY_8_PORT", "8."), ("TEST_VALKEY_9_PORT", "9.")] {
        let state = AppState::default();
        let opened = open(&state, port_var).await;

        assert_eq!(opened.server.flavor, ServerFlavor::Valkey);
        let version = opened.server.version.clone().unwrap();
        assert!(version.starts_with(major), "{port_var}: {version}");
        // Valkey 8 起 redis_version 固定报 7.2.4，按它判会判反
        assert_ne!(opened.server.version, opened.server.compat_version);
        assert_eq!(
            state_of(&opened, CapabilityId::ExpireConditions),
            CapabilityState::Supported
        );
        assert_eq!(
            state_of(&opened, CapabilityId::SetKeepTtl),
            CapabilityState::Supported
        );
        assert_save_keeps_ttl(&state, &opened.connection_id).await;
        close_connection(&state, &opened.connection_id)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[serial]
#[ignore = "requires the e2e.ps1 server matrix"]
async fn keydb_is_recognised_and_unregistered_arguments_stay_unknown() {
    let state = AppState::default();
    let opened = open(&state, "TEST_KEYDB_PORT").await;

    assert_eq!(opened.server.flavor, ServerFlavor::Keydb);
    // KeyDB 不在 since 表里：没把握就是 unknown，按各项的「未知时」策略处理
    let expire = &opened.capabilities[&CapabilityId::ExpireConditions];
    assert_eq!(expire.state, CapabilityState::Unknown);
    assert!(!expire.usable);
    let keep = &opened.capabilities[&CapabilityId::SetKeepTtl];
    assert_eq!(keep.state, CapabilityState::Unknown);
    assert!(keep.usable);
    // 未知时按 supported 发 KEEPTTL —— KeyDB 6.3 实际支持它
    assert_save_keeps_ttl(&state, &opened.connection_id).await;
    close_connection(&state, &opened.connection_id)
        .await
        .unwrap();
}

#[tokio::test]
#[serial]
#[ignore = "requires the e2e.ps1 server matrix"]
async fn dragonfly_is_recognised_by_its_own_version_field() {
    let state = AppState::default();
    let opened = open(&state, "TEST_DRAGONFLY_PORT").await;

    assert_eq!(opened.server.flavor, ServerFlavor::Dragonfly);
    let version = opened.server.version.clone().unwrap();
    assert!(
        version.chars().next().is_some_and(|c| c.is_ascii_digit()),
        "df-v 前缀要剥掉：{version}"
    );
    assert_eq!(
        state_of(&opened, CapabilityId::Scripting),
        CapabilityState::Supported
    );
    assert_eq!(
        state_of(&opened, CapabilityId::ExpireConditions),
        CapabilityState::Unknown
    );
    assert_save_keeps_ttl(&state, &opened.connection_id).await;
    close_connection(&state, &opened.connection_id)
        .await
        .unwrap();
}

/* ── STRING 保存的三条路径 ────────────────────────────── */

#[tokio::test]
#[serial]
#[ignore = "requires the e2e.ps1 server matrix"]
async fn redis_5_saves_through_the_script_and_keeps_the_ttl() {
    let state = AppState::default();
    let opened = open(&state, "TEST_REDIS_5_PORT").await;

    assert_eq!(
        opened.server.version.as_deref().map(|v| &v[..2]),
        Some("5.")
    );
    assert_eq!(
        state_of(&opened, CapabilityId::SetKeepTtl),
        CapabilityState::Unsupported
    );
    assert_eq!(
        state_of(&opened, CapabilityId::Scripting),
        CapabilityState::Supported
    );
    assert_save_keeps_ttl(&state, &opened.connection_id).await;

    // 没有 TTL 的 key 保存后仍然没有 TTL —— 脚本只在 ttl > 0 时补 PEXPIRE
    let key = format!("cap:persist:{}", uuid::Uuid::new_v4());
    cli(&state, &opened.connection_id, &format!("SET {key} before")).await;
    set_string_value(&state, &opened.connection_id, &key, &STANDARD.encode(b"x"))
        .await
        .unwrap();
    assert_eq!(pttl(&state, &opened.connection_id, &key).await, -1);
    cli(&state, &opened.connection_id, &format!("DEL {key}")).await;
    close_connection(&state, &opened.connection_id)
        .await
        .unwrap();
}

#[tokio::test]
#[serial]
#[ignore = "requires the e2e.ps1 server matrix"]
async fn redis_5_refuses_to_save_when_scripting_is_also_unusable() {
    let state = AppState::default();
    let opened = open(&state, "TEST_REDIS_5_PORT").await;
    let id = &opened.connection_id;
    {
        let sessions = state.sessions.lock().await;
        sessions.get(id).unwrap().runtime.set_capability(
            CapabilityId::Scripting,
            Capability {
                state: CapabilityState::Unsupported,
                usable: false,
                reason: "forced by test".into(),
            },
        );
    }

    let key = format!("cap:refuse:{}", uuid::Uuid::new_v4());
    cli(&state, id, &format!("SET {key} before EX 600")).await;
    let saved = set_string_value(&state, id, &key, &STANDARD.encode(b"after")).await;
    let value = get(&state, id, &key).await;
    let ttl = pttl(&state, id, &key).await;
    cli(&state, id, &format!("DEL {key}")).await;

    // 绝不退回裸 SET 悄悄清掉 TTL：值不变、TTL 不变、明确报错
    let payload = saved.expect_err("save must be refused").payload();
    assert_eq!(payload.code, "redis.capability_unsupported");
    assert_eq!(value, "before");
    assert!(ttl > 590_000);
    close_connection(&state, id).await.unwrap();
}

/* ── 运行时降级 ───────────────────────────────────────── */

#[tokio::test]
#[serial]
#[ignore = "requires the e2e.ps1 server matrix"]
async fn an_unknown_keepttl_rejected_by_the_server_degrades_without_retrying() {
    let state = AppState::default();
    let events = recording_sink(&state);
    let opened = open(&state, "TEST_REDIS_5_PORT").await;
    let id = &opened.connection_id;
    // 模拟认不出的兼容实现：setKeepTtl 未知时照常发 KEEPTTL
    force_capability(
        &state,
        id,
        CapabilityId::SetKeepTtl,
        CapabilityState::Unknown,
    )
    .await;

    let key = format!("cap:degrade:{}", uuid::Uuid::new_v4());
    cli(&state, id, &format!("SET {key} before EX 600")).await;
    let first = set_string_value(&state, id, &key, &STANDARD.encode(b"after")).await;
    let untouched = get(&state, id, &key).await;

    // 本次按失败报告，不换路径悄悄重做
    let payload = first
        .expect_err("KEEPTTL is a syntax error on Redis 5")
        .payload();
    assert_eq!(payload.code, "redis.capability_unsupported");
    assert_eq!(untouched, "before");
    {
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].connection_id, *id);
        assert_eq!(events[0].id, CapabilityId::SetKeepTtl);
        assert_eq!(events[0].capability.state, CapabilityState::Unsupported);
    }

    // 下一次保存走脚本路径，TTL 照样保留
    set_string_value(&state, id, &key, &STANDARD.encode(b"after"))
        .await
        .unwrap();
    assert!(pttl(&state, id, &key).await > 590_000);
    cli(&state, id, &format!("DEL {key}")).await;
    close_connection(&state, id).await.unwrap();
}

#[tokio::test]
#[serial]
#[ignore = "requires the e2e.ps1 server matrix"]
async fn redis_6_has_keepttl_but_no_expire_conditions() {
    let state = AppState::default();
    let events = recording_sink(&state);
    let opened = open(&state, "TEST_REDIS_LEGACY_PORT").await;
    let id = &opened.connection_id;

    let expire = &opened.capabilities[&CapabilityId::ExpireConditions];
    assert_eq!(expire.state, CapabilityState::Unsupported);
    assert!(
        expire.reason.contains("需要 Redis 7.0 及以上"),
        "{}",
        expire.reason
    );
    assert_eq!(
        state_of(&opened, CapabilityId::SetKeepTtl),
        CapabilityState::Supported
    );
    assert_save_keeps_ttl(&state, id).await;

    // 判据写错时（这里强行判成可用）：整批同样失败，按任务失败报告并降级，
    // 而不是记成一串 failed
    force_capability(
        &state,
        id,
        CapabilityId::ExpireConditions,
        CapabilityState::Supported,
    )
    .await;
    let key = format!("cap:nx:{}", uuid::Uuid::new_v4());
    cli(&state, id, &format!("SET {key} v")).await;
    let spec = TtlSpec {
        mode: TtlMode::Relative,
        value: Some(600),
        condition: Some(TtlCondition::Nx),
    };
    let result =
        apply_ttl_for_connection(&state, id, std::slice::from_ref(&key), None, &spec, false).await;
    cli(&state, id, &format!("DEL {key}")).await;
    close_connection(&state, id).await.unwrap();

    let error = result.expect_err("EXPIRE NX is unknown on Redis 6");
    assert_eq!(error.payload().code, "redis.capability_unsupported");
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, CapabilityId::ExpireConditions);
}

/* ── 权限 ─────────────────────────────────────────────── */

/// 在单机 Redis 7 上建一个 ACL 用户并以它的身份开会话。`reset` 让上一次断言失败
/// 残留的同名用户不影响这一次；断言前先拆用户。
async fn open_as_acl_user(
    name: &str,
    rules: &str,
) -> (AppState, Result<ConnectionOpened, AppError>) {
    let admin_state = AppState::default();
    let admin = open(&admin_state, "TEST_REDIS_PORT").await;
    let line = format!("ACL SETUSER {name} reset on >pw ~* &* {rules}");
    cli(&admin_state, &admin.connection_id, &line).await;

    let state = AppState::default();
    let mut user = config("TEST_REDIS_PORT");
    user.username = Some(name.into());
    user.auth = Some("pw".into());
    let opened = open_connection(&state, user).await;
    (state, opened)
}

async fn drop_acl_user(name: &str) {
    let state = AppState::default();
    let admin = open(&state, "TEST_REDIS_PORT").await;
    cli(&state, &admin.connection_id, &format!("ACL DELUSER {name}")).await;
}

#[tokio::test]
#[serial]
#[ignore = "requires the e2e.ps1 server matrix"]
async fn a_user_denied_command_still_connects_with_command_capabilities_unknown() {
    let (_state, opened) = open_as_acl_user("cap-nocommand", "+@all -command").await;
    drop_acl_user("cap-nocommand").await;

    let opened = opened.expect("a denied COMMAND is not a reason to fail the connection");
    let scripting = &opened.capabilities[&CapabilityId::Scripting];
    assert_eq!(scripting.state, CapabilityState::Unknown);
    assert!(scripting.usable);
    assert!(scripting.reason.contains("ACL"), "{}", scripting.reason);
    // 参数级能力的判据是版本号，不受 COMMAND 被拒影响
    assert_eq!(
        state_of(&opened, CapabilityId::ExpireConditions),
        CapabilityState::Supported
    );
}

#[tokio::test]
#[serial]
#[ignore = "requires the e2e.ps1 server matrix"]
async fn a_denied_command_is_a_permission_error_not_an_unsupported_one() {
    let (state, opened) = open_as_acl_user("cap-noexpire", "+@all -expire -set").await;
    let outcome = async {
        let opened = opened.expect("ACL user should connect");
        let id = opened.connection_id.clone();
        let key = format!("cap:acl:{}", uuid::Uuid::new_v4());
        let ttl = set_key_ttl(&state, &id, &key, TtlMode::Relative, Some(60)).await;
        // 脚本里被拒的是脚本调用的那条命令，不是 EVAL
        let value: CreateKeyValue = serde_json::from_value(
            serde_json::json!({"type": "string", "valueBase64": STANDARD.encode(b"v")}),
        )
        .unwrap();
        let created = create_key(&state, &id, &key, value, KeyExpiry::Persist).await;
        (opened, ttl, created)
    }
    .await;
    drop_acl_user("cap-noexpire").await;
    let (opened, ttl, created) = outcome;

    // 能力不看权限：服务端支持 EXPIRE NX，只是这个用户不能执行
    assert_eq!(
        state_of(&opened, CapabilityId::ExpireConditions),
        CapabilityState::Supported
    );
    let error = ttl.expect_err("EXPIRE is denied for this user");
    assert!(
        matches!(&error, AppError::PermissionDenied { command: Some(command), .. } if command == "EXPIRE"),
        "{error:?}"
    );
    assert_eq!(error.payload().message, "当前用户无权执行 `EXPIRE`");
    let error = created.expect_err("SET inside the script is denied");
    assert!(
        matches!(&error, AppError::PermissionDenied { command: Some(command), .. } if command == "SET"),
        "{error:?}"
    );
}
