use serial_test::serial;
use tsubasa_redis_manager_lib::models::{ConnectionConfig, KeyEntry, RedisValue, TaskStatus};
use tsubasa_redis_manager_lib::services::connection::{
    close_connection, execute_cli, open_connection,
};
use tsubasa_redis_manager_lib::services::task::{
    scan_and_annotate_one_page, scan_one_page, ScanProgressEstimator,
};
use tsubasa_redis_manager_lib::state::AppState;

fn redis_host() -> String {
    std::env::var("PHASE3A_REDIS_HOST").unwrap_or_else(|_| "127.0.0.1".into())
}

fn redis_port() -> u16 {
    std::env::var("PHASE3A_REDIS_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(6379)
}

fn config() -> ConnectionConfig {
    ConnectionConfig {
        host: redis_host(),
        port: redis_port(),
        auth: None,
        username: None,
        db: Some(15),
        name: Some("phase3a".into()),
        separator: Some(":".into()),
        connection_read_only: Some(false),
        cluster: None,
        ssl_options: None,
        ssh_options: None,
        sentinel_options: None,
    }
}

async fn seed_keys(state: &AppState, connection_id: &str, keys: &[String]) -> Result<(), String> {
    for key in keys {
        execute_cli(state, connection_id, &format!("DEL {key}"))
            .await
            .map_err(|err| format!("initial cleanup failed for {key}: {err}"))?;
        execute_cli(state, connection_id, &format!("SET {key} value"))
            .await
            .map_err(|err| format!("SET failed for {key}: {err}"))?;
    }
    Ok(())
}

async fn cleanup_keys(
    state: &AppState,
    connection_id: &str,
    keys: &[String],
) -> Result<(), String> {
    let mut errors = Vec::new();
    for key in keys {
        if let Err(err) = execute_cli(state, connection_id, &format!("DEL {key}")).await {
            errors.push(format!("{key}: {err}"));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

async fn scan_all_matching_keys(
    state: &AppState,
    connection_id: &str,
    pattern: &str,
) -> Result<Vec<String>, String> {
    let runtime = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(connection_id)
            .map(|session| session.runtime.clone())
            .ok_or_else(|| format!("connection not found: {connection_id}"))?
    };

    let mut cursor = "0".to_string();
    let mut found = Vec::new();
    let mut executor = runtime.executor.lock().await;
    for _ in 0..1000 {
        let (next_cursor, mut keys) = scan_one_page(&mut executor, &cursor, Some(pattern), 50)
            .await
            .map_err(|err| format!("scan_one_page failed: {err}"))?;
        found.append(&mut keys);
        if next_cursor == "0" {
            found.sort();
            found.dedup();
            return Ok(found);
        }
        cursor = next_cursor;
    }

    Err("scan cursor did not complete before iteration limit".into())
}

async fn scan_and_annotate_all_matching_keys(
    state: &AppState,
    connection_id: &str,
    pattern: &str,
    with_ttl: bool,
) -> Result<Vec<KeyEntry>, String> {
    let runtime = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(connection_id)
            .map(|session| session.runtime.clone())
            .ok_or_else(|| format!("connection not found: {connection_id}"))?
    };

    let mut cursor = "0".to_string();
    let mut found: Vec<KeyEntry> = Vec::new();
    // 顺带断言进度估算的契约（doc/backend/tasks.md「扫描任务：只报命中数」）：真实服务端回的游标序列喂进估算器，
    // 结果必须单调不减、游标归零时收在 1.0。
    let mut estimator = ScanProgressEstimator::standalone();
    let mut last_estimate = 0.0_f64;
    let mut executor = runtime.executor.lock().await;
    for _ in 0..1000 {
        let (next_cursor, mut entries) =
            scan_and_annotate_one_page(&mut executor, &cursor, Some(pattern), 50, with_ttl)
                .await
                .map_err(|err| format!("scan_and_annotate_one_page failed: {err}"))?;
        found.append(&mut entries);
        if let Some(estimate) = estimator.observe(&next_cursor) {
            if estimate < last_estimate {
                return Err(format!(
                    "scan progress estimate regressed: {estimate} < {last_estimate}"
                ));
            }
            last_estimate = estimate;
        }
        if next_cursor == "0" {
            if last_estimate != 1.0 {
                return Err(format!(
                    "estimate should settle at 1.0 on the final page, got {last_estimate}"
                ));
            }
            found.sort_by(|a, b| a.key.cmp(&b.key));
            found.dedup_by(|a, b| a.key == b.key);
            return Ok(found);
        }
        cursor = next_cursor;
    }

    Err("scan cursor did not complete before iteration limit".into())
}

/// `search_keys` / `load_all_keys` 任务的核心页循环（`run_scan_task` 逐页调的
/// 就是 `scan_and_annotate_one_page`）。任务的启动入口要 `AppHandle`，e2e 里
/// 没有 —— 与上面 `scan_one_page` 的先例一致，直接驱动真正接触服务端的这一层。
/// 断言的是 `doc/backend/tasks.md`「事件项带 keyType」的契约：
/// 事件项的 key 自带类型，`withTtl` 开着时 TTL 同批到达。
#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn search_keys_task_page_loop_annotates_types_and_ttl_from_local_redis() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let pattern = format!("phase3a:annotate:{token}:*");
    let persistent_key = format!("phase3a:annotate:{token}:persistent");
    let expiring_key = format!("phase3a:annotate:{token}:expiring");
    let cleanup_targets = vec![persistent_key.clone(), expiring_key.clone()];

    let scan_result = async {
        seed_keys(&state, &opened.connection_id, &cleanup_targets).await?;
        execute_cli(
            &state,
            &opened.connection_id,
            &format!("EXPIRE {expiring_key} 3600"),
        )
        .await
        .map_err(|err| format!("EXPIRE failed for {expiring_key}: {err}"))?;
        scan_and_annotate_all_matching_keys(&state, &opened.connection_id, &pattern, true).await
    }
    .await;
    let cleanup_result = cleanup_keys(&state, &opened.connection_id, &cleanup_targets).await;
    let close_result = close_connection(&state, &opened.connection_id).await;

    cleanup_result.expect("test keys should be removed before assertions");
    close_result.expect("connection should close before assertions");

    let entries = scan_result.expect("annotated scan should complete for the phase3a pattern");
    assert_eq!(
        entries.iter().map(|entry| &entry.key).collect::<Vec<_>>(),
        vec![&expiring_key, &persistent_key]
    );
    for entry in &entries {
        assert_eq!(
            entry.key_type.as_deref(),
            Some("string"),
            "seeded keys are strings, so should the annotation be for {}",
            entry.key
        );
    }
    let expiring = &entries[0];
    let persistent = &entries[1];
    assert!(
        expiring.ttl.is_some_and(|ttl| ttl > 0 && ttl <= 3600),
        "expiring key should carry its remaining TTL, got {:?}",
        expiring.ttl
    );
    assert_eq!(
        persistent.ttl,
        Some(-1),
        "persistent key should report TTL -1"
    );
}

#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn scan_one_page_reads_phase3a_pattern_from_local_redis() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let pattern = format!("phase3a:scan:{token}:*");
    let keys = vec![
        format!("phase3a:scan:{token}:one"),
        format!("phase3a:scan:{token}:two"),
    ];
    let control_key = format!("phase3a:control:{token}");
    let mut cleanup_targets = keys.clone();
    cleanup_targets.push(control_key.clone());

    let scan_result = async {
        seed_keys(&state, &opened.connection_id, &cleanup_targets).await?;
        scan_all_matching_keys(&state, &opened.connection_id, &pattern).await
    }
    .await;
    let cleanup_result = cleanup_keys(&state, &opened.connection_id, &cleanup_targets).await;
    let close_result = close_connection(&state, &opened.connection_id).await;

    cleanup_result.expect("test keys should be removed before assertions");
    close_result.expect("connection should close before assertions");

    let found = scan_result.expect("scan_one_page should scan the Phase 3A pattern");
    let mut expected = keys;
    expected.sort();
    assert_eq!(found, expected);
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn phase3a_vds_batch_delete_preserves_non_matching_keys() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let delete_key = format!("phase3a:vds:delete:{token}:yes");
    let keep_key = format!("phase3a:vds:delete:{token}:no");
    let cleanup_targets = vec![delete_key.clone(), keep_key.clone()];

    let operation_result = async {
        // VDS batch-delete prerequisite baseline: direct Redis operations verify
        // matching and non-matching keys are distinguishable without requiring a Tauri AppHandle.
        execute_cli(
            &state,
            &opened.connection_id,
            &format!("SET {delete_key} 1"),
        )
        .await
        .map_err(|err| format!("delete key should seed: {err}"))?;
        execute_cli(&state, &opened.connection_id, &format!("SET {keep_key} 1"))
            .await
            .map_err(|err| format!("keep key should seed: {err}"))?;
        execute_cli(&state, &opened.connection_id, &format!("DEL {delete_key}"))
            .await
            .map_err(|err| format!("matching key should delete: {err}"))?;

        let delete_exists = execute_cli(
            &state,
            &opened.connection_id,
            &format!("EXISTS {delete_key}"),
        )
        .await
        .map_err(|err| format!("delete key existence should be queryable: {err}"))?;
        let keep_exists = execute_cli(&state, &opened.connection_id, &format!("EXISTS {keep_key}"))
            .await
            .map_err(|err| format!("keep key existence should be queryable: {err}"))?;

        Ok::<_, String>((delete_exists, keep_exists))
    }
    .await;
    let cleanup_result = cleanup_keys(&state, &opened.connection_id, &cleanup_targets).await;
    let close_result = close_connection(&state, &opened.connection_id).await;

    cleanup_result.expect("test keys should be removed before assertions");
    close_result.expect("connection should close before assertions");

    let (delete_exists, keep_exists) =
        operation_result.expect("batch delete should only remove matching keys");
    assert!(matches!(delete_exists.value, RedisValue::Integer(0)));
    assert!(matches!(keep_exists.value, RedisValue::Integer(1)));
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn phase3a_vds_task_registry_records_cancellation_status() {
    let state = AppState::default();
    let started = tsubasa_redis_manager_lib::services::task::register_task(
        &state,
        "conn-vds".into(),
        "phase3a-vds".into(),
        "load_all_keys".into(),
    )
    .await;

    let cancelled =
        tsubasa_redis_manager_lib::services::task::cancel_task(&state, &started.task_id)
            .await
            .expect("task should cancel");
    let queried =
        tsubasa_redis_manager_lib::services::task::get_task_status(&state, &started.task_id)
            .await
            .expect("task status should be queryable");

    assert_eq!(cancelled.status, TaskStatus::Cancelled);
    assert_eq!(queried.status, TaskStatus::Cancelled);
}
