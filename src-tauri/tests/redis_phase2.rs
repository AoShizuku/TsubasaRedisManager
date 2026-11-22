use serial_test::serial;
use tsubasa_redis_manager_lib::models::{
    ConnectionConfig, RedisValue, SentinelOptions, SshOptions, SslOptions,
};
use tsubasa_redis_manager_lib::services::connection::{
    close_connection, execute_cli, get_key_meta, open_connection, scan_keys,
};
use tsubasa_redis_manager_lib::services::task::ScanProgressEstimator;
use tsubasa_redis_manager_lib::services::values::{
    delete_keys, get_list_range, get_string_value, set_string_value,
};
use tsubasa_redis_manager_lib::state::AppState;

const LOCAL_VDS_ENV_HELP: &str = "Copy scripts/ci/local-vds.env.example.ps1 to scripts/ci/local-vds.env.ps1 for ignored VDS E2E tests.";

fn env_string(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.into())
}

fn optional_env_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn required_env_string(key: &str, help: &str) -> String {
    optional_env_string(key).unwrap_or_else(|| {
        panic!("{key} must be set to run ignored VDS E2E tests. {help}");
    })
}

fn env_port(key: &str, default: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(default)
}

fn phase2_host(mode: &str) -> String {
    let mode_key = format!("PHASE2_{}_HOST", mode);
    optional_env_string(&mode_key)
        .or_else(|| optional_env_string("PHASE2_HOST"))
        .unwrap_or_else(|| {
            panic!(
                "{mode_key} or PHASE2_HOST must be set to run ignored VDS E2E tests. {LOCAL_VDS_ENV_HELP}"
            );
        })
}

fn base_config(mode: &str, port: u16) -> ConnectionConfig {
    ConnectionConfig {
        host: phase2_host(mode),
        port,
        auth: None,
        username: None,
        db: None,
        name: Some(format!("phase2-{mode}")),
        separator: Some(":".into()),
        connection_read_only: Some(false),
        cluster: None,
        ssl_options: None,
        ssh_options: None,
        sentinel_options: None,
    }
}

fn standalone_config() -> ConnectionConfig {
    base_config("STANDALONE", env_port("PHASE2_STANDALONE_PORT", 16379))
}

fn tls_options(default_servername: &str) -> SslOptions {
    SslOptions {
        key: Some(env_string(
            "PHASE2_TLS_KEY",
            "../scripts/vds-phase2-compose/certs/client.key",
        )),
        cert: Some(env_string(
            "PHASE2_TLS_CERT",
            "../scripts/vds-phase2-compose/certs/client.crt",
        )),
        ca: Some(env_string(
            "PHASE2_TLS_CA",
            "../scripts/vds-phase2-compose/certs/ca.crt",
        )),
        servername: Some(env_string("PHASE2_TLS_SERVERNAME", default_servername)),
        insecure: Some(env_bool("PHASE2_TLS_INSECURE", true)),
    }
}

fn tls_config() -> ConnectionConfig {
    let host = phase2_host("TLS");
    let mut config = base_config("TLS", env_port("PHASE2_TLS_PORT", 16380));
    config.host = host.clone();
    config.ssl_options = Some(tls_options(&host));
    config
}

fn ssh_shared_options() -> SshOptions {
    SshOptions {
        host: phase2_host("SSH"),
        port: env_port("PHASE2_SSH_PORT", 12222),
        username: env_string("PHASE2_SSH_USER", "redis"),
        password: None,
        privatekey: None,
        privatekeybookmark: None,
        passphrase: None,
        timeout: Some(env_port("PHASE2_SSH_TIMEOUT_SECONDS", 30) as u64),
    }
}

fn ssh_password_options() -> SshOptions {
    SshOptions {
        password: Some(required_env_string(
            "PHASE2_SSH_PASSWORD",
            LOCAL_VDS_ENV_HELP,
        )),
        ..ssh_shared_options()
    }
}

fn ssh_key_options() -> SshOptions {
    SshOptions {
        password: None,
        privatekey: Some(env_string(
            "PHASE2_SSH_KEY",
            "../scripts/vds-phase2-compose/ssh/id_ed25519",
        )),
        ..ssh_shared_options()
    }
}

fn ssh_standalone_config(ssh_options: SshOptions) -> ConnectionConfig {
    let mut config = base_config("SSH_STANDALONE", 6379);
    config.host = env_string("PHASE2_SSH_STANDALONE_TARGET_HOST", "redis-standalone");
    config.port = env_port("PHASE2_SSH_STANDALONE_TARGET_PORT", 6379);
    config.ssh_options = Some(ssh_options);
    config
}

fn tls_over_ssh_config() -> ConnectionConfig {
    let mut config = base_config("TLS_OVER_SSH", 6379);
    config.host = env_string("PHASE2_TLS_SSH_TARGET_HOST", "redis-tls");
    config.port = env_port("PHASE2_TLS_SSH_TARGET_PORT", 6379);
    config.ssl_options = Some(SslOptions {
        key: Some(env_string(
            "PHASE2_TLS_KEY",
            "../scripts/vds-phase2-compose/certs/client.key",
        )),
        cert: Some(env_string(
            "PHASE2_TLS_CERT",
            "../scripts/vds-phase2-compose/certs/client.crt",
        )),
        ca: Some(env_string(
            "PHASE2_TLS_CA",
            "../scripts/vds-phase2-compose/certs/ca.crt",
        )),
        servername: Some(env_string("PHASE2_TLS_SSH_SERVERNAME", "redis-tls")),
        insecure: Some(env_bool("PHASE2_TLS_SSH_INSECURE", false)),
    });
    config.ssh_options = Some(ssh_password_options());
    config
}

fn cluster_config() -> ConnectionConfig {
    let mut config = base_config("CLUSTER", env_port("PHASE2_CLUSTER_PORT", 17000));
    config.cluster = Some(true);
    config
}

fn sentinel_config() -> ConnectionConfig {
    let mut config = base_config("SENTINEL", env_port("PHASE2_SENTINEL_PORT", 26379));
    config.sentinel_options = Some(SentinelOptions {
        master_name: env_string("PHASE2_SENTINEL_MASTER", "mymaster"),
        node_password: None,
    });
    config
}

fn cluster_ssh_config() -> ConnectionConfig {
    let mut config = base_config("CLUSTER_SSH", 17000);
    config.host = env_string("PHASE2_CLUSTER_SSH_SEED_HOST", "cluster-1");
    config.port = env_port("PHASE2_CLUSTER_SSH_SEED_PORT", 17000);
    config.cluster = Some(true);
    config.ssh_options = Some(ssh_password_options());
    config
}

fn sentinel_ssh_config() -> ConnectionConfig {
    let mut config = base_config("SENTINEL_SSH", 26379);
    config.host = env_string("PHASE2_SENTINEL_SSH_HOST", "redis-sentinel");
    config.port = env_port("PHASE2_SENTINEL_SSH_PORT", 26379);
    config.sentinel_options = Some(SentinelOptions {
        master_name: env_string("PHASE2_SENTINEL_MASTER", "mymaster"),
        node_password: None,
    });
    config.ssh_options = Some(ssh_password_options());
    config
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn new(keys: &[&'static str]) -> Self {
        let saved = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

fn catch_expected_panic<F, T>(operation: F) -> String
where
    F: FnOnce() -> T + std::panic::UnwindSafe,
{
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(operation);
    std::panic::set_hook(previous_hook);

    let payload = match result {
        Ok(_) => panic!("operation should panic"),
        Err(payload) => payload,
    };

    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        message.to_string()
    } else {
        "non-string panic".into()
    }
}

#[test]
#[serial]
fn phase2_host_requires_explicit_env_when_missing() {
    let _guard = EnvGuard::new(&["PHASE2_STANDALONE_HOST", "PHASE2_HOST"]);
    std::env::remove_var("PHASE2_STANDALONE_HOST");
    std::env::remove_var("PHASE2_HOST");

    let message = catch_expected_panic(|| phase2_host("STANDALONE"));

    assert!(message.contains("PHASE2_STANDALONE_HOST or PHASE2_HOST"));
    assert!(message.contains("scripts/ci/local-vds.env.example.ps1"));
}

#[test]
#[serial]
fn phase2_host_requires_explicit_env_when_blank() {
    let _guard = EnvGuard::new(&["PHASE2_STANDALONE_HOST", "PHASE2_HOST"]);
    std::env::set_var("PHASE2_STANDALONE_HOST", " ");
    std::env::set_var("PHASE2_HOST", "");

    let message = catch_expected_panic(|| phase2_host("STANDALONE"));

    assert!(message.contains("PHASE2_STANDALONE_HOST or PHASE2_HOST"));
    assert!(message.contains("scripts/ci/local-vds.env.ps1"));
}

#[test]
#[serial]
fn phase2_ssh_password_requires_explicit_env() {
    let _guard = EnvGuard::new(&["PHASE2_SSH_PASSWORD"]);
    std::env::remove_var("PHASE2_SSH_PASSWORD");

    let message = catch_expected_panic(|| {
        required_env_string("PHASE2_SSH_PASSWORD", LOCAL_VDS_ENV_HELP);
    });

    assert!(message.contains("PHASE2_SSH_PASSWORD"));
    assert!(message.contains("scripts/ci/local-vds.env.example.ps1"));
}

#[test]
#[serial]
fn phase2_ssh_key_options_do_not_require_password_env() {
    let _guard = EnvGuard::new(&[
        "PHASE2_SSH_HOST",
        "PHASE2_HOST",
        "PHASE2_SSH_KEY",
        "PHASE2_SSH_PASSWORD",
    ]);
    std::env::set_var("PHASE2_SSH_HOST", "ssh.example.invalid");
    std::env::remove_var("PHASE2_HOST");
    std::env::set_var("PHASE2_SSH_KEY", "fixtures/id_ed25519");
    std::env::remove_var("PHASE2_SSH_PASSWORD");

    let options = ssh_key_options();

    assert_eq!(options.host, "ssh.example.invalid");
    assert_eq!(options.password, None);
    assert_eq!(options.privatekey.as_deref(), Some("fixtures/id_ed25519"));
}

async fn assert_ping(config: ConnectionConfig) {
    let state = AppState::default();
    let opened = open_connection(&state, config)
        .await
        .expect("connection should open");

    let result = execute_cli(&state, &opened.connection_id, "PING").await;
    let close_result = close_connection(&state, &opened.connection_id).await;

    close_result.expect("connection should close");
    let result = result.expect("PING should execute");
    assert_eq!(result.command, "PING");
    assert!(matches!(result.value, RedisValue::String(value) if value == "PONG"));
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn standalone_redis_connection_on_vds_exposed_port_pings() {
    assert_ping(standalone_config()).await;
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn tls_redis_connection_on_vds_exposed_port_pings() {
    assert_ping(tls_config()).await;
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn ssh_standalone_connection_with_password_pings() {
    assert_ping(ssh_standalone_config(ssh_password_options())).await;
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn ssh_standalone_connection_with_private_key_pings() {
    assert_ping(ssh_standalone_config(ssh_key_options())).await;
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn tls_over_ssh_internal_redis_pings() {
    assert_ping(tls_over_ssh_config()).await;
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn redis_cluster_connection_on_vds_exposed_port_pings() {
    assert_ping(cluster_config()).await;
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn redis_cluster_scan_reads_keys_from_all_masters() {
    let state = AppState::default();
    let opened = open_connection(&state, cluster_config())
        .await
        .expect("cluster connection should open");
    let keys = (0..30)
        .map(|index| format!("phase2:cluster-scan:{index}"))
        .collect::<Vec<_>>();

    for key in &keys {
        let _ = execute_cli(&state, &opened.connection_id, &format!("DEL {key}")).await;
        execute_cli(&state, &opened.connection_id, &format!("SET {key} ok"))
            .await
            .expect("cluster test key should be written");
    }

    // 顺带断言进度估算的契约（doc/backend/tasks.md「扫描任务：只报命中数」）：真实集群回的合成游标喂进估算器，
    // 结果必须单调不减、扫完收在 1.0。这条消费路径单元测试只喂得了
    // 手工构造的游标，真实的节点数 / DBSIZE / 游标形状只有这里才有。
    let runtime = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&opened.connection_id)
            .map(|session| session.runtime.clone())
            .expect("cluster session should exist")
    };
    // for_executor 内部自己短持锁（CLUSTER NODES 与每节点 DBSIZE 各一次）——
    // 这里顺带验证它在真实集群上采集权重时不与调用方争锁。
    let mut estimator = ScanProgressEstimator::for_executor(&runtime.executor).await;
    let mut last_estimate = 0.0_f64;

    let mut cursor = "0".to_string();
    let mut found = Vec::new();
    let mut completed = false;
    for _ in 0..200 {
        // with_ttl = true 会让每个节点多收一批 TTL。集群下这批 pipeline 必须
        // 分节点发 —— 跨槽位的批会被整个打到错误的节点上，所以这里顺带
        // 覆盖了 PipelineTarget::Node 那条路径。
        let page = scan_keys(
            &state,
            &opened.connection_id,
            &cursor,
            Some("phase2:cluster-scan:*".into()),
            Some(1),
            true,
        )
        .await
        .expect("cluster scan should read a page");
        if let Some(estimate) = estimator.observe(&page.cursor) {
            assert!(
                estimate >= last_estimate,
                "cluster scan estimate regressed: {estimate} < {last_estimate}"
            );
            last_estimate = estimate;
        }
        for entry in &page.keys {
            assert_eq!(
                entry.key_type.as_deref(),
                Some("string"),
                "cluster scan should carry the type for {}",
                entry.key
            );
            assert_eq!(
                entry.ttl,
                Some(-1),
                "cluster scan should carry the ttl for {}",
                entry.key
            );
        }
        found.extend(page.keys.into_iter().map(|entry| entry.key));
        if page.completed {
            completed = true;
            break;
        }
        cursor = page.cursor;
    }

    for key in &keys {
        let _ = execute_cli(&state, &opened.connection_id, &format!("DEL {key}")).await;
    }
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    found.sort();
    found.dedup();
    assert!(completed, "cluster scan cursor did not complete");
    assert_eq!(
        last_estimate, 1.0,
        "estimate should settle at 1.0 once every node cursor returns to zero"
    );
    for key in keys {
        assert!(found.contains(&key), "cluster scan missed {key}");
    }
}

/// 集群下的 `get_key_meta`。
///
/// 它走的是 `PipelineTarget::Key`（`Route::with_key` 现算槽位），
/// 和分节点 SCAN 那条 `PipelineTarget::Node` 是**两种不同的路由方式**。
/// 路由错了结果照样正确（redis-rs 跟随 MOVED 转发），只是每条命令多一跳；
/// 这里验证的是元信息在集群下取得全。
#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn redis_cluster_key_meta_routes_the_pipeline_to_the_slot_owner() {
    let state = AppState::default();
    let opened = open_connection(&state, cluster_config())
        .await
        .expect("cluster connection should open");
    let token = uuid::Uuid::new_v4();

    // 刻意用互不相同的前缀，让这些 key 散落到不同的主节点上
    let keys = (0..12)
        .map(|index| format!("phase2:meta:{token}:{index}"))
        .collect::<Vec<_>>();

    let result = async {
        for key in &keys {
            execute_cli(&state, &opened.connection_id, &format!("SET {key} value"))
                .await
                .map_err(|err| format!("seeding {key} should succeed: {err}"))?;
            execute_cli(&state, &opened.connection_id, &format!("EXPIRE {key} 600"))
                .await
                .map_err(|err| format!("setting a ttl on {key} should succeed: {err}"))?;
        }

        for key in &keys {
            let meta = get_key_meta(&state, &opened.connection_id, key)
                .await
                .map_err(|err| format!("reading meta for {key} should succeed: {err}"))?;

            if meta.key_type.as_deref() != Some("string") {
                return Err(format!(
                    "{key}: expected a string type, got {:?}",
                    meta.key_type
                ));
            }
            if meta.ttl <= 0 {
                return Err(format!("{key}: expected a positive ttl, got {}", meta.ttl));
            }
            // STRLEN 走在同一批 pipeline 里，落空说明路由没打中槽位持有者
            if meta.size != Some(5) {
                return Err(format!("{key}: expected size 5, got {:?}", meta.size));
            }
            // 集群下必须有槽位；单机才是 None
            if meta.slot.is_none() {
                return Err(format!("{key}: cluster keys must report a slot"));
            }
        }

        Ok::<_, String>(())
    }
    .await;

    let cleanup = delete_keys(&state, &opened.connection_id, &keys).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");
    cleanup.expect("test keys should be removed");
    result.expect("cluster key meta should route to the slot owner");
}

/// 集群下按下标删除 List 用的 Lua 脚本。
///
/// `EVAL` 在集群下按 key 路由，脚本本身在服务端原子执行 ——
/// 前端 v0.1 那种分两条发 `LSET` 哨兵 + `LREM` 的做法在并发写时会删错元素。
#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn redis_cluster_deletes_a_list_item_by_index_atomically() {
    let state = AppState::default();
    let opened = open_connection(&state, cluster_config())
        .await
        .expect("cluster connection should open");
    let token = uuid::Uuid::new_v4();
    let key = format!("phase2:list:{token}");

    let result = async {
        for item in ["a", "b", "c", "d"] {
            execute_cli(
                &state,
                &opened.connection_id,
                &format!("RPUSH {key} {item}"),
            )
            .await
            .map_err(|err| format!("seeding the list should succeed: {err}"))?;
        }

        // 删中间那个，两端都不能受影响
        let deleted = tsubasa_redis_manager_lib::services::values::delete_list_item(
            &state,
            &opened.connection_id,
            &key,
            1,
        )
        .await
        .map_err(|err| format!("deleting index 1 should succeed: {err}"))?;
        if !deleted {
            return Err("delete_list_item reported that nothing was removed".to_string());
        }

        let page = get_list_range(&state, &opened.connection_id, &key, 0, -1)
            .await
            .map_err(|err| format!("reading the list back should succeed: {err}"))?;
        let items = page
            .items
            .iter()
            .map(|item| item.text.clone().unwrap_or_default())
            .collect::<Vec<_>>();
        if items != vec!["a", "c", "d"] {
            return Err(format!(
                "expected [a, c, d] after the delete, got {items:?}"
            ));
        }
        if page.total != 3 {
            return Err(format!("expected a total of 3, got {}", page.total));
        }

        // 越界下标必须是"没删到"而不是报错
        let missing = tsubasa_redis_manager_lib::services::values::delete_list_item(
            &state,
            &opened.connection_id,
            &key,
            99,
        )
        .await
        .map_err(|err| format!("an out-of-range delete should not error: {err}"))?;
        if missing {
            return Err("an out-of-range index should report nothing removed".to_string());
        }

        Ok::<_, String>(())
    }
    .await;

    let cleanup = delete_keys(&state, &opened.connection_id, &[key]).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");
    cleanup.expect("test key should be removed");
    result.expect("atomic list delete should work on a cluster");
}

/// 非 UTF-8 的值必须原样往返，而且不能被当成文本。
#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn redis_cluster_round_trips_non_utf8_values_as_bytes() {
    let state = AppState::default();
    let opened = open_connection(&state, cluster_config())
        .await
        .expect("cluster connection should open");
    let token = uuid::Uuid::new_v4();
    let key = format!("phase2:bytes:{token}");
    // gzip 魔数打头，外加一个非法 UTF-8 续接字节
    let payload: &[u8] = &[0x1f, 0x8b, 0x08, 0x00, 0xff, 0xfe, 0x41];
    let payload_base64 = base64_encode(payload);

    let result = async {
        set_string_value(&state, &opened.connection_id, &key, &payload_base64)
            .await
            .map_err(|err| format!("writing raw bytes should succeed: {err}"))?;

        let stored = get_string_value(&state, &opened.connection_id, &key)
            .await
            .map_err(|err| format!("reading raw bytes should succeed: {err}"))?;

        if stored.text.is_some() {
            return Err("non-UTF-8 bytes must not come back as text".to_string());
        }
        if stored.base64.as_deref() != Some(payload_base64.as_str()) {
            return Err(format!(
                "bytes changed in transit: sent {payload_base64}, got {:?}",
                stored.base64
            ));
        }
        if stored.total_bytes.is_some() {
            return Err("a 7-byte value must not be reported as truncated".to_string());
        }
        Ok::<_, String>(())
    }
    .await;

    let cleanup = delete_keys(&state, &opened.connection_id, &[key]).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");
    cleanup.expect("test key should be removed");
    result.expect("non-UTF-8 values should survive a round trip");
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn redis_sentinel_connection_on_vds_exposed_port_pings() {
    assert_ping(sentinel_config()).await;
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn cluster_over_ssh_internal_seed_pings() {
    assert_ping(cluster_ssh_config()).await;
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn sentinel_over_ssh_internal_sentinel_pings() {
    assert_ping(sentinel_ssh_config()).await;
}
