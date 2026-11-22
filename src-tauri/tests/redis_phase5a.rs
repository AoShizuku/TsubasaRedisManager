//! v0.3 批量 TTL 的端到端测试。
//!
//! 单元测试喂的是手工构造的 `redis::Value`，发现不了这里真正会出错的东西：
//! 服务端对 `EXPIRE` 返回 0 还是 1、`NX` 在低版本上直接报错、以及集群下
//! 一批 key 到底被打到了哪个节点。最后一条单看结果是看不出来的：打错节点的 `EXPIRE`
//! 会被 redis-rs 跟着 MOVED 转发到持有槽位的节点，TTL 照样设上，只是每条多一跳。
//! 路由对不对要看服务端的 `rejected_calls`，见 `redis_phase5b.rs` 的 `rejected_del_calls`。
//!
//! 任务的启动入口要 `AppHandle`，这里没有，所以直接驱动它下面那层
//! （`apply_ttl_batch` / `apply_ttl_for_connection` / `scan_one_page_with_nodes`）。

use serial_test::serial;
use tsubasa_redis_manager_lib::models::{ConnectionConfig, RedisValue, TtlCondition, TtlMode};
use tsubasa_redis_manager_lib::services::capabilities::CapabilityId;
use tsubasa_redis_manager_lib::services::connection::{
    close_connection, execute_cli, open_connection,
};
use tsubasa_redis_manager_lib::services::task::{
    apply_ttl_batch, apply_ttl_for_connection, scan_one_page_with_nodes, BatchTtlProgress, TtlSpec,
};
use tsubasa_redis_manager_lib::state::AppState;

const LOCAL_VDS_ENV_HELP: &str = "Copy scripts/ci/local-vds.env.example.ps1 to scripts/ci/local-vds.env.ps1 for ignored VDS E2E tests.";

fn optional_env_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn redis_host() -> String {
    optional_env_string("PHASE5A_REDIS_HOST")
        .or_else(|| optional_env_string("PHASE2_HOST"))
        .unwrap_or_else(|| {
            panic!(
                "PHASE5A_REDIS_HOST or PHASE2_HOST must be set to run ignored VDS E2E tests. {LOCAL_VDS_ENV_HELP}"
            );
        })
}

fn env_port(key: &str, fallback: &str, default: u16) -> u16 {
    std::env::var(key)
        .or_else(|_| std::env::var(fallback))
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn config() -> ConnectionConfig {
    ConnectionConfig {
        host: redis_host(),
        port: env_port("PHASE5A_REDIS_PORT", "PHASE2_STANDALONE_PORT", 16379),
        auth: None,
        username: None,
        db: Some(12),
        name: Some("phase5a".into()),
        separator: Some(":".into()),
        connection_read_only: Some(false),
        cluster: None,
        ssl_options: None,
        ssh_options: None,
        sentinel_options: None,
    }
}

fn cluster_config() -> ConnectionConfig {
    let mut config = config();
    config.host = optional_env_string("PHASE5A_CLUSTER_HOST")
        .or_else(|| optional_env_string("PHASE2_CLUSTER_HOST"))
        .unwrap_or_else(redis_host);
    config.port = env_port("PHASE5A_CLUSTER_PORT", "PHASE2_CLUSTER_PORT", 17000);
    // 集群没有 db 的概念
    config.db = None;
    config.cluster = Some(true);
    config.name = Some("phase5a-cluster".into());
    config
}

fn relative(seconds: i64) -> TtlSpec {
    TtlSpec {
        mode: TtlMode::Relative,
        value: Some(seconds),
        condition: None,
    }
}

async fn seed(state: &AppState, connection_id: &str, key: &str) -> Result<(), String> {
    execute_cli(state, connection_id, &format!("SET {key} value"))
        .await
        .map(|_| ())
        .map_err(|err| format!("SET {key} failed: {err}"))
}

async fn ttl_of(state: &AppState, connection_id: &str, key: &str) -> Result<i64, String> {
    let result = execute_cli(state, connection_id, &format!("TTL {key}"))
        .await
        .map_err(|err| format!("TTL {key} failed: {err}"))?;
    match result.value {
        RedisValue::Integer(ttl) => Ok(ttl),
        other => Err(format!("TTL {key} returned an unexpected shape: {other:?}")),
    }
}

async fn cleanup(state: &AppState, connection_id: &str, keys: &[String]) {
    for key in keys {
        let _ = execute_cli(state, connection_id, &format!("DEL {key}")).await;
    }
}

/// 直接对一批 key 跑一次批量 TTL，落点交给调用方。
async fn apply(
    state: &AppState,
    connection_id: &str,
    keys: &[String],
    node: Option<(&str, u16)>,
    spec: &TtlSpec,
) -> Result<BatchTtlProgress, String> {
    let runtime = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(connection_id)
            .map(|session| session.runtime.clone())
            .ok_or_else(|| format!("connection not found: {connection_id}"))?
    };
    let mut executor = runtime.executor.lock().await;
    apply_ttl_batch(&mut executor, keys, node, spec)
        .await
        .map_err(|err| format!("apply_ttl_batch failed: {err}"))
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn batch_ttl_sets_matching_keys_and_leaves_the_rest_alone() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let inside = (0..3)
        .map(|index| format!("phase5a:ttl:{token}:in:{index}"))
        .collect::<Vec<_>>();
    let outside = format!("phase5a:ttl:{token}:out");
    let mut targets = inside.clone();
    targets.push(outside.clone());

    let result = async {
        for key in &targets {
            seed(&state, &opened.connection_id, key).await?;
        }

        // 走真实的扫描路径拿到这一页，而不是把 key 名直接喂进去 ——
        // 要覆盖的正是"扫描出来的东西能不能原样交给写入"。
        let page = scan_one_page_with_nodes(
            &mut *state
                .sessions
                .lock()
                .await
                .get(&opened.connection_id)
                .expect("session")
                .runtime
                .executor
                .lock()
                .await,
            "0",
            Some(&format!("phase5a:ttl:{token}:in:*")),
            200,
        )
        .await
        .map_err(|err| format!("scan failed: {err}"))?;

        let scanned = page
            .batches
            .iter()
            .flat_map(|batch| batch.keys.clone())
            .collect::<Vec<_>>();
        if scanned.len() != inside.len() {
            return Err(format!(
                "expected the scan to find {} keys, got {scanned:?}",
                inside.len()
            ));
        }

        let progress = apply(
            &state,
            &opened.connection_id,
            &scanned,
            None,
            &relative(600),
        )
        .await?;

        let mut ttls = Vec::new();
        for key in &inside {
            ttls.push(ttl_of(&state, &opened.connection_id, key).await?);
        }
        let untouched = ttl_of(&state, &opened.connection_id, &outside).await?;

        Ok::<_, String>((progress, ttls, untouched))
    }
    .await;

    cleanup(&state, &opened.connection_id, &targets).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    let (progress, ttls, untouched) = result.expect("batch ttl should run");
    assert_eq!(progress.processed, 3);
    assert_eq!(progress.succeeded, 3);
    assert_eq!(progress.failed, 0);
    for ttl in ttls {
        assert!(
            (1..=600).contains(&ttl),
            "expected a live countdown, got {ttl}"
        );
    }
    // -1 = 永不过期。模式外的 key 一根汗毛都不能动。
    assert_eq!(untouched, -1);
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn batch_ttl_counts_a_missing_key_as_unchanged_not_failed() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let present = format!("phase5a:ttl:{token}:present");
    let missing = format!("phase5a:ttl:{token}:missing");
    let targets = vec![present.clone(), missing.clone()];

    let result = async {
        seed(&state, &opened.connection_id, &present).await?;
        let _ = execute_cli(&state, &opened.connection_id, &format!("DEL {missing}")).await;

        apply(
            &state,
            &opened.connection_id,
            &targets,
            None,
            &relative(600),
        )
        .await
    }
    .await;

    cleanup(&state, &opened.connection_id, &targets).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    let progress = result.expect("batch ttl should run");
    // EXPIRE 对不存在的 key 返回 0。那既不是成功也不是失败 ——
    // 算进 failed 会让界面报一个不存在的错误，见 doc/backend/tasks.md「进度口径」。
    assert_eq!(progress.processed, 2);
    assert_eq!(progress.succeeded, 1);
    assert_eq!(progress.failed, 0);
    assert_eq!(progress.processed - progress.succeeded - progress.failed, 1);
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn batch_ttl_nx_refuses_to_overwrite_an_existing_expiry() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");

    let conditions = &opened.capabilities[&CapabilityId::ExpireConditions];
    if !conditions.usable {
        close_connection(&state, &opened.connection_id)
            .await
            .expect("connection should close");
        eprintln!("skipping: {}", conditions.reason);
        return;
    }

    let token = uuid::Uuid::new_v4();
    let already_set = format!("phase5a:ttl:{token}:has-expiry");
    let no_expiry = format!("phase5a:ttl:{token}:no-expiry");
    let targets = vec![already_set.clone(), no_expiry.clone()];

    let result = async {
        for key in &targets {
            seed(&state, &opened.connection_id, key).await?;
        }
        execute_cli(
            &state,
            &opened.connection_id,
            &format!("EXPIRE {already_set} 300"),
        )
        .await
        .map_err(|err| format!("seeding an expiry failed: {err}"))?;

        let spec = TtlSpec {
            mode: TtlMode::Relative,
            value: Some(9000),
            condition: Some(TtlCondition::Nx),
        };
        let progress = apply(&state, &opened.connection_id, &targets, None, &spec).await?;

        let kept = ttl_of(&state, &opened.connection_id, &already_set).await?;
        let fresh = ttl_of(&state, &opened.connection_id, &no_expiry).await?;
        Ok::<_, String>((progress, kept, fresh))
    }
    .await;

    cleanup(&state, &opened.connection_id, &targets).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    let (progress, kept, fresh) = result.expect("batch ttl with NX should run");
    // 没有 NX 的话，这一批会把 300 秒的那个也改成 9000 —— 这正是
    // 「给 cache:* 补 TTL」误伤生产数据的形状，见 doc/frontend/batch.md「条件 flag」。
    assert!(
        (1..=300).contains(&kept),
        "NX must not overwrite the existing 300s expiry, got {kept}"
    );
    assert!(
        (301..=9000).contains(&fresh),
        "the key without an expiry should have been set, got {fresh}"
    );
    assert_eq!(progress.processed, 2);
    assert_eq!(progress.succeeded, 1);
    assert_eq!(progress.failed, 0);
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn batch_ttl_persist_clears_an_expiry_and_reports_untouched_keys() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let expiring = format!("phase5a:ttl:{token}:expiring");
    let permanent = format!("phase5a:ttl:{token}:permanent");
    let targets = vec![expiring.clone(), permanent.clone()];

    let result = async {
        for key in &targets {
            seed(&state, &opened.connection_id, key).await?;
        }
        execute_cli(
            &state,
            &opened.connection_id,
            &format!("EXPIRE {expiring} 300"),
        )
        .await
        .map_err(|err| format!("seeding an expiry failed: {err}"))?;

        let spec = TtlSpec {
            mode: TtlMode::Persist,
            value: None,
            condition: None,
        };
        let progress = apply(&state, &opened.connection_id, &targets, None, &spec).await?;

        let cleared = ttl_of(&state, &opened.connection_id, &expiring).await?;
        Ok::<_, String>((progress, cleared))
    }
    .await;

    cleanup(&state, &opened.connection_id, &targets).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    let (progress, cleared) = result.expect("batch persist should run");
    assert_eq!(cleared, -1, "PERSIST should have removed the expiry");
    // PERSIST 对本来就没有 TTL 的 key 返回 0 —— 未变更，不是失败
    assert_eq!(progress.processed, 2);
    assert_eq!(progress.succeeded, 1);
    assert_eq!(progress.failed, 0);
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn batch_ttl_dry_run_counts_without_touching_anything() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let keys = (0..4)
        .map(|index| format!("phase5a:dry:{token}:{index}"))
        .collect::<Vec<_>>();

    let result = async {
        for key in &keys {
            seed(&state, &opened.connection_id, key).await?;
        }

        let dry = apply_ttl_for_connection(
            &state,
            &opened.connection_id,
            &keys,
            None,
            &relative(600),
            true,
        )
        .await
        .map_err(|err| format!("dry run failed: {err}"))?;

        let mut after_dry_run = Vec::new();
        for key in &keys {
            after_dry_run.push(ttl_of(&state, &opened.connection_id, key).await?);
        }

        let real = apply_ttl_for_connection(
            &state,
            &opened.connection_id,
            &keys,
            None,
            &relative(600),
            false,
        )
        .await
        .map_err(|err| format!("real run failed: {err}"))?;

        Ok::<_, String>((dry, after_dry_run, real))
    }
    .await;

    cleanup(&state, &opened.connection_id, &keys).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    let (dry, after_dry_run, real) = result.expect("dry run should work");
    // 试算的全部价值在于"试算出来的数字就是真会改的数字"
    assert_eq!(dry.processed, real.processed);
    assert_eq!(dry.succeeded, 0, "a dry run must not claim any change");
    for ttl in after_dry_run {
        assert_eq!(ttl, -1, "a dry run must not have set an expiry");
    }
    assert_eq!(real.succeeded, 4);
}

/// 集群下批量 TTL 的路由。
///
/// 跨槽位的一批 pipeline 会被 redis-rs 按**第一条命令**整个路由出去，其余的全部先落到
/// 不持有该槽位的节点上，被 MOVED 拒掉后再逐条转发——结果正确，但每条多一跳，
/// 按节点整批发的收益全部抵消。
///
/// 这里验证分节点扫描回来的每一批都带着落点，且按落点发出去之后 TTL 真的设上了。
/// 「设上了」证明不了路由对；路由的断言在 `redis_phase5b.rs` 的批量删除用例里
/// （同一条 `query_pipeline_raw` 路径，看 `rejected_calls`）。
#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn cluster_batch_ttl_routes_each_page_to_the_node_it_came_from() {
    let state = AppState::default();
    let opened = open_connection(&state, cluster_config())
        .await
        .expect("cluster connection should open");
    let token = uuid::Uuid::new_v4();
    // 30 个 key 足以散布到三个主节点上
    let keys = (0..30)
        .map(|index| format!("phase5a:cluster:{token}:{index}"))
        .collect::<Vec<_>>();

    let result = async {
        for key in &keys {
            seed(&state, &opened.connection_id, key).await?;
        }

        let mut cursor = "0".to_string();
        let mut progress = BatchTtlProgress::default();
        let mut batch_count = 0_usize;
        for _ in 0..200 {
            let page = {
                let runtime = {
                    let sessions = state.sessions.lock().await;
                    sessions
                        .get(&opened.connection_id)
                        .map(|session| session.runtime.clone())
                        .ok_or_else(|| "connection not found".to_string())?
                };
                let mut executor = runtime.executor.lock().await;
                scan_one_page_with_nodes(
                    &mut executor,
                    &cursor,
                    Some(&format!("phase5a:cluster:{token}:*")),
                    10,
                )
                .await
                .map_err(|err| format!("cluster scan failed: {err}"))?
            };

            for batch in &page.batches {
                let (host, port) = batch
                    .node
                    .as_ref()
                    .ok_or_else(|| "a cluster scan batch must carry its node".to_string())?;
                batch_count += 1;
                let outcome = apply(
                    &state,
                    &opened.connection_id,
                    &batch.keys,
                    Some((host.as_str(), *port)),
                    &relative(600),
                )
                .await?;
                progress.processed += outcome.processed;
                progress.succeeded += outcome.succeeded;
                progress.failed += outcome.failed;
            }

            cursor = page.cursor;
            if cursor == "0" {
                break;
            }
        }

        let mut ttls = Vec::new();
        for key in &keys {
            ttls.push(ttl_of(&state, &opened.connection_id, key).await?);
        }
        Ok::<_, String>((progress, batch_count, ttls))
    }
    .await;

    cleanup(&state, &opened.connection_id, &keys).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    let (progress, batch_count, ttls) = result.expect("cluster batch ttl should run");
    assert!(
        batch_count > 1,
        "the fixture should spread across more than one master, got {batch_count} batch(es)"
    );
    assert_eq!(progress.processed, 30);
    // 每一条都必须真的设上
    assert_eq!(
        progress.succeeded, 30,
        "some EXPIRE landed on a node that does not own the slot"
    );
    assert_eq!(progress.failed, 0);
    for (key, ttl) in keys.iter().zip(ttls) {
        assert!(
            (1..=600).contains(&ttl),
            "{key} should have a live countdown, got {ttl}"
        );
    }
}

/// 集群下的显式 key 列表（用户勾选来的）跨槽位，没有统一落点。
///
/// 那批不能 pipeline —— `apply_ttl_batch` 据此退化成逐 key 串行。
/// 这条测的就是"跨槽位的一批交给它也不会出错"。
#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn cluster_batch_ttl_falls_back_to_one_key_at_a_time_without_a_node() {
    let state = AppState::default();
    let opened = open_connection(&state, cluster_config())
        .await
        .expect("cluster connection should open");
    let token = uuid::Uuid::new_v4();
    let keys = (0..12)
        .map(|index| format!("phase5a:picked:{token}:{index}"))
        .collect::<Vec<_>>();

    let result = async {
        for key in &keys {
            seed(&state, &opened.connection_id, key).await?;
        }
        // node = None：这正是勾选列表的形状
        let progress = apply(&state, &opened.connection_id, &keys, None, &relative(600)).await?;

        let mut ttls = Vec::new();
        for key in &keys {
            ttls.push(ttl_of(&state, &opened.connection_id, key).await?);
        }
        Ok::<_, String>((progress, ttls))
    }
    .await;

    cleanup(&state, &opened.connection_id, &keys).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    let (progress, ttls) = result.expect("cluster explicit-list batch ttl should run");
    assert_eq!(progress.processed, 12);
    assert_eq!(progress.succeeded, 12);
    assert_eq!(progress.failed, 0);
    for ttl in ttls {
        assert!((1..=600).contains(&ttl), "expected a countdown, got {ttl}");
    }
}
