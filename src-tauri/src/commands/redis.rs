use crate::error::AppResult;
use crate::models::{
    BatchDeleteTaskOptions, BatchTtlTaskOptions, CommandLogEvent, CommandResult, ConnectionConfig,
    ConnectionOpened, ConnectionStatusEvent, ExportKeysTaskOptions, KeyListResult, KeyMeta,
    KeyScanTaskOptions, MemoryAnalysisTaskOptions, RedisCommandArg, RedisText,
    SubscriptionTaskOptions, TaskStarted, TaskStatusSnapshot, TtlMode,
};
use crate::services::credential_store::{connection_store_error, KeyringCredentialStore};
use crate::services::values;
use crate::state::AppState;
use std::path::PathBuf;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};

#[tauri::command]
pub async fn redis_start_clone_key(
    app: AppHandle,
    state: State<'_, AppState>,
    source_connection_id: String,
    target_connection_id: String,
    source_key: String,
    target_key: String,
    expiry: values::KeyExpiry,
) -> AppResult<TaskStarted> {
    values::start_clone_task(
        Some(std::sync::Arc::new(move |event| {
            let _ = app.emit("redis://task-progress", event);
        })),
        state.inner(),
        &source_connection_id,
        &target_connection_id,
        &source_key,
        &target_key,
        expiry,
    )
    .await
}

#[tauri::command]
pub async fn redis_get_clone_task(
    state: State<'_, AppState>,
    task_id: String,
) -> AppResult<crate::models::CloneTaskSnapshot> {
    values::get_clone_task(state.inner(), &task_id).await
}

#[tauri::command]
pub async fn redis_key_exists(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
) -> AppResult<bool> {
    values::key_exists(state.inner(), &connection_id, &key).await
}

#[tauri::command]
pub async fn redis_create_key(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    value: values::CreateKeyValue,
    expiry: values::KeyExpiry,
) -> AppResult<values::KeyWriteResult> {
    values::create_key(state.inner(), &connection_id, &key, value, expiry).await
}

#[tauri::command]
pub async fn redis_open_connection(
    state: State<'_, AppState>,
    config: ConnectionConfig,
) -> AppResult<ConnectionOpened> {
    crate::services::connection::open_connection(state.inner(), config).await
}

#[tauri::command]
pub async fn redis_open_stored_connection(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_key: String,
    db: Option<i64>,
) -> AppResult<ConnectionOpened> {
    let path = connections_path(&app)?;
    let secrets = KeyringCredentialStore;
    let config = crate::services::connection_store::resolve_connection_config(
        path,
        &secrets,
        &connection_key,
        db,
    )?;
    let connection_name = connection_name_for_config(&config);

    emit_connection_status(
        &app,
        connection_status_event(
            None,
            Some(&connection_key),
            &connection_name,
            "connecting",
            None,
            None,
        ),
    );

    let result = crate::services::connection::open_connection(state.inner(), config).await;
    match result {
        Ok(opened) => {
            emit_connection_status(
                &app,
                connection_status_event(
                    Some(&opened.connection_id),
                    Some(&connection_key),
                    &opened.connection_name,
                    "ready",
                    None,
                    None,
                ),
            );
            Ok(opened)
        }
        Err(err) => {
            let payload = err.payload();
            emit_connection_status(
                &app,
                connection_status_event(
                    None,
                    Some(&connection_key),
                    &connection_name,
                    "failed",
                    Some(&payload.code),
                    Some(&payload.message),
                ),
            );
            Err(err)
        }
    }
}

#[tauri::command]
pub async fn redis_close_connection(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<()> {
    let connection_name = state
        .sessions
        .lock()
        .await
        .get(&connection_id)
        .map(|session| session.connection_name.clone())
        .unwrap_or_else(|| connection_id.clone());

    crate::services::connection::close_connection(state.inner(), &connection_id).await?;
    emit_connection_status(
        &app,
        connection_status_event(
            Some(&connection_id),
            None,
            &connection_name,
            "closed",
            None,
            None,
        ),
    );
    Ok(())
}

#[tauri::command]
pub async fn redis_execute_cli(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    command_line: String,
) -> AppResult<CommandResult> {
    let started = Instant::now();
    let (command, args) = summarize_cli_for_log(&command_line);
    let result =
        crate::services::connection::execute_cli(state.inner(), &connection_id, &command_line)
            .await;
    emit_command_log(
        &app,
        state.inner(),
        &connection_id,
        command,
        args,
        result
            .as_ref()
            .map(|result| result.elapsed_ms)
            .unwrap_or_else(|_| started.elapsed().as_millis()),
        if result.is_ok() { "success" } else { "error" },
    )
    .await;
    result
}

#[tauri::command]
/// 返回的每个 key 都带类型。`with_ttl` 会额外补一批 `TTL` —— 每 key 多一条命令，
/// 成本翻倍，所以由调用方按设置项决定要不要，默认不取。
pub async fn redis_scan_keys(
    state: State<'_, AppState>,
    connection_id: String,
    cursor: String,
    pattern: Option<String>,
    count: Option<usize>,
    with_ttl: Option<bool>,
) -> AppResult<KeyListResult> {
    crate::services::connection::scan_keys(
        state.inner(),
        &connection_id,
        &cursor,
        pattern,
        count,
        with_ttl.unwrap_or(false),
    )
    .await
}

#[tauri::command]
pub async fn redis_get_key_meta(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
) -> AppResult<KeyMeta> {
    crate::services::connection::get_key_meta(state.inner(), &connection_id, &key).await
}

/* ── 按类型划分的取值命令 ─────────────────────────────
参数形状按每种类型真实的分页语义设计，不做统一抽象。
见 doc/backend/ipc-contract.md「参数形状按分页语义」。 */

#[tauri::command]
pub async fn redis_get_string_value(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
) -> AppResult<RedisText> {
    values::get_string_value(state.inner(), &connection_id, &key).await
}

#[tauri::command]
pub async fn redis_get_hash_entries(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    cursor: String,
    pattern: Option<String>,
    count: Option<usize>,
) -> AppResult<values::HashPage> {
    values::get_hash_entries(state.inner(), &connection_id, &key, &cursor, pattern, count).await
}

#[tauri::command]
pub async fn redis_get_set_members(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    cursor: String,
    pattern: Option<String>,
    count: Option<usize>,
) -> AppResult<values::MemberPage> {
    values::get_set_members(state.inner(), &connection_id, &key, &cursor, pattern, count).await
}

#[tauri::command]
pub async fn redis_get_list_range(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    start: i64,
    stop: i64,
) -> AppResult<values::ListPage> {
    values::get_list_range(state.inner(), &connection_id, &key, start, stop).await
}

#[tauri::command]
pub async fn redis_get_zset_range(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    start: i64,
    stop: i64,
    rev: Option<bool>,
) -> AppResult<values::ZSetPage> {
    values::get_zset_range(
        state.inner(),
        &connection_id,
        &key,
        start,
        stop,
        rev.unwrap_or(false),
    )
    .await
}

#[tauri::command]
pub async fn redis_get_stream_entries(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    start: String,
    end: String,
    count: Option<usize>,
    rev: Option<bool>,
) -> AppResult<values::StreamPage> {
    values::get_stream_entries(
        state.inner(),
        &connection_id,
        &key,
        &start,
        &end,
        count,
        rev.unwrap_or(false),
    )
    .await
}

#[tauri::command]
pub async fn redis_get_stream_groups(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
) -> AppResult<Vec<values::StreamGroup>> {
    values::get_stream_groups(state.inner(), &connection_id, &key).await
}

/// HLL 的估算基数（PFCOUNT）。对非 HLL 值原样透传服务端的 WRONGTYPE ——
/// 前端靠这个错误进入"只读原始字节"的回退态。
#[tauri::command]
pub async fn redis_get_hll_count(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
) -> AppResult<u64> {
    values::get_hll_count(state.inner(), &connection_id, &key).await
}

/// HLL 结构查看器的数据：头部字段 + 展开成 16,384 个值的寄存器数组。
#[tauri::command]
pub async fn redis_inspect_hll(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
) -> AppResult<crate::services::hll::HllInspect> {
    values::inspect_hll(state.inner(), &connection_id, &key).await
}

/// 按定位符重取一个完整的值，上限 8 MB。
///
/// 集合成员与 Hash 字段在分页里是按 64 KiB 截断的（`MEMBER_VALUE_LIMIT`），
/// 而半截 gzip 跟半截 msgpack 一样解不开 —— 想解码就得先有完整字节。
///
/// **必须由用户显式触发**，不是把分页的上限放大：一页 200 条各 8 MB
/// 是 1.6 GB。见 `doc/backend/ipc-contract.md`「完整值重取」。
#[tauri::command]
pub async fn redis_get_full_value(
    state: State<'_, AppState>,
    connection_id: String,
    target: values::ValueRef,
) -> AppResult<RedisText> {
    values::get_full_value(state.inner(), &connection_id, &target).await
}

/* ── 写入命令 ─────────────────────────────────────────
只读性由这些命令自身保证：每一条在服务层入口显式判 session.readonly，
不经过 readonly.rs 的命令名匹配。见 doc/backend/sessions.md「只读属于会话」。 */

#[tauri::command]
pub async fn redis_set_string_value(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    value_base64: String,
) -> AppResult<()> {
    values::set_string_value(state.inner(), &connection_id, &key, &value_base64).await
}

#[tauri::command]
pub async fn redis_set_hash_field(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    field_base64: String,
    value_base64: String,
) -> AppResult<()> {
    values::set_hash_field(
        state.inner(),
        &connection_id,
        &key,
        &field_base64,
        &value_base64,
    )
    .await
}

#[tauri::command]
pub async fn redis_delete_hash_fields(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    fields_base64: Vec<String>,
) -> AppResult<u64> {
    values::delete_hash_fields(state.inner(), &connection_id, &key, &fields_base64).await
}

#[tauri::command]
pub async fn redis_add_set_members(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    members_base64: Vec<String>,
) -> AppResult<u64> {
    values::add_set_members(state.inner(), &connection_id, &key, &members_base64).await
}

#[tauri::command]
pub async fn redis_delete_set_members(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    members_base64: Vec<String>,
) -> AppResult<u64> {
    values::delete_set_members(state.inner(), &connection_id, &key, &members_base64).await
}

#[tauri::command]
pub async fn redis_set_list_item(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    index: i64,
    value_base64: String,
) -> AppResult<()> {
    values::set_list_item(state.inner(), &connection_id, &key, index, &value_base64).await
}

#[tauri::command]
pub async fn redis_push_list_item(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    value_base64: String,
    tail: bool,
) -> AppResult<u64> {
    values::push_list_item(state.inner(), &connection_id, &key, &value_base64, tail).await
}

/// 按下标删除。Redis 没有这个命令，后端用 Lua 把 LSET 哨兵 + LREM 包成原子操作 ——
/// 分两条命令发的话，中间的并发写会让 LREM 删到别的元素上。
#[tauri::command]
pub async fn redis_delete_list_item(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    index: i64,
) -> AppResult<bool> {
    values::delete_list_item(state.inner(), &connection_id, &key, index).await
}

#[tauri::command]
pub async fn redis_add_zset_member(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    member_base64: String,
    score: String,
) -> AppResult<()> {
    values::add_zset_member(state.inner(), &connection_id, &key, &member_base64, &score).await
}

#[tauri::command]
pub async fn redis_delete_zset_members(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    members_base64: Vec<String>,
) -> AppResult<u64> {
    values::delete_zset_members(state.inner(), &connection_id, &key, &members_base64).await
}

/// PFADD。返回"基数是否变化"，前端据此决定要不要刷新计数。
#[tauri::command]
pub async fn redis_add_hll_elements(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    elements_base64: Vec<String>,
) -> AppResult<bool> {
    values::add_hll_elements(state.inner(), &connection_id, &key, &elements_base64).await
}

#[tauri::command]
pub async fn redis_set_key_ttl(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    mode: TtlMode,
    value: Option<i64>,
) -> AppResult<()> {
    values::set_key_ttl(state.inner(), &connection_id, &key, mode, value).await
}

#[tauri::command]
pub async fn redis_rename_key(
    state: State<'_, AppState>,
    connection_id: String,
    key: String,
    new_key: String,
) -> AppResult<()> {
    values::rename_key(state.inner(), &connection_id, &key, &new_key).await
}

#[tauri::command]
pub async fn redis_delete_keys(
    state: State<'_, AppState>,
    connection_id: String,
    keys: Vec<String>,
) -> AppResult<u64> {
    values::delete_keys(state.inner(), &connection_id, &keys).await
}

/// DB 列表。`CONFIG GET databases` 拿不到时（ACL / rename-command）
/// 后端自己退回 `INFO keyspace` 推断，前端不需要知道这条回退链。
#[tauri::command]
pub async fn redis_get_database_list(
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<Vec<values::DatabaseInfo>> {
    values::get_database_list(state.inner(), &connection_id).await
}

#[tauri::command]
pub async fn redis_get_server_memory(
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<values::ServerMemory> {
    values::get_server_memory(state.inner(), &connection_id).await
}

#[tauri::command]
pub async fn redis_cancel_task(
    state: State<'_, AppState>,
    task_id: String,
) -> AppResult<TaskStatusSnapshot> {
    crate::services::task::cancel_task(state.inner(), &task_id).await
}

#[tauri::command]
pub async fn redis_get_task_status(
    state: State<'_, AppState>,
    task_id: String,
) -> AppResult<TaskStatusSnapshot> {
    crate::services::task::get_task_status(state.inner(), &task_id).await
}

/// 暂停 / 恢复一个任务（窗口隐藏时暂停、显示时恢复的后端半边）。
///
/// 暂停是**运行时标志**而不是新的任务状态，理由见
/// `services/task.rs::set_task_paused`。目前只有扫描任务
/// （`search_keys` / `load_all_keys`）在页间检查它。
#[tauri::command]
pub async fn redis_set_task_paused(
    state: State<'_, AppState>,
    task_id: String,
    paused: bool,
) -> AppResult<TaskStatusSnapshot> {
    crate::services::task::set_task_paused(state.inner(), &task_id, paused).await
}

#[tauri::command]
pub async fn redis_start_load_all_keys(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    options: KeyScanTaskOptions,
) -> AppResult<TaskStarted> {
    crate::services::task::start_load_all_keys_task(app, state.inner(), connection_id, options)
        .await
}

#[tauri::command]
pub async fn redis_start_search_keys(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    options: KeyScanTaskOptions,
) -> AppResult<TaskStarted> {
    crate::services::task::start_search_keys_task(app, state.inner(), connection_id, options).await
}

#[tauri::command]
pub async fn redis_start_batch_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    options: BatchDeleteTaskOptions,
) -> AppResult<TaskStarted> {
    crate::services::task::start_batch_delete_task(app, state.inner(), connection_id, options).await
}

#[tauri::command]
pub async fn redis_start_batch_ttl(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    options: BatchTtlTaskOptions,
) -> AppResult<TaskStarted> {
    crate::services::task::start_batch_ttl_task(app, state.inner(), connection_id, options).await
}

#[tauri::command]
pub async fn redis_start_memory_analysis(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    options: MemoryAnalysisTaskOptions,
) -> AppResult<TaskStarted> {
    crate::services::task::start_memory_analysis_task(app, state.inner(), connection_id, options)
        .await
}

#[tauri::command]
pub async fn redis_start_monitor(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<TaskStarted> {
    crate::services::task::start_monitor_task(app, state.inner(), connection_id).await
}

#[tauri::command]
pub async fn redis_start_subscription(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    options: SubscriptionTaskOptions,
) -> AppResult<TaskStarted> {
    crate::services::task::start_subscription_task(app, state.inner(), connection_id, options).await
}

#[tauri::command]
pub async fn redis_start_import_keys(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    file_path: String,
) -> AppResult<TaskStarted> {
    crate::services::task::start_import_keys_task(app, state.inner(), connection_id, file_path)
        .await
}

#[tauri::command]
pub async fn redis_start_import_commands(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    file_path: String,
) -> AppResult<TaskStarted> {
    crate::services::task::start_import_commands_task(app, state.inner(), connection_id, file_path)
        .await
}

#[tauri::command]
pub async fn redis_start_export_keys(
    app: AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    options: ExportKeysTaskOptions,
) -> AppResult<TaskStarted> {
    crate::services::task::start_export_keys_task(app, state.inner(), connection_id, options).await
}

fn summarize_cli_for_log(command_line: &str) -> (String, String) {
    let Ok(args) = crate::services::cli::parse_cli(command_line) else {
        return ("".into(), "".into());
    };
    let Some(command) = args.first() else {
        return ("".into(), "".into());
    };
    let command = command.to_string();
    let args = args
        .into_iter()
        .skip(1)
        .map(|value| RedisCommandArg::String { value })
        .collect::<Vec<_>>();
    let args = crate::services::events::summarize_args(&command, &args);
    (command, args)
}

fn connection_status_event(
    connection_id: Option<&str>,
    connection_key: Option<&str>,
    connection_name: &str,
    status: &str,
    code: Option<&str>,
    message: Option<&str>,
) -> ConnectionStatusEvent {
    ConnectionStatusEvent {
        connection_id: connection_id.map(str::to_string),
        connection_key: connection_key.map(str::to_string),
        connection_name: connection_name.to_string(),
        status: status.to_string(),
        code: code.map(str::to_string),
        message: message.map(str::to_string),
    }
}

fn emit_connection_status(app: &AppHandle, event: ConnectionStatusEvent) {
    let _ = app.emit("redis://connection-status", event);
}

fn connections_path(app: &AppHandle) -> AppResult<PathBuf> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("connections.json"))
        .map_err(|err| connection_store_error("connection_store.load_failed", err.to_string()))
}

fn connection_name_for_config(config: &ConnectionConfig) -> String {
    config
        .name
        .as_ref()
        .filter(|name| !name.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| format!("{}:{}/{}", config.host, config.port, config.db.unwrap_or(0)))
}

async fn emit_command_log(
    app: &AppHandle,
    state: &AppState,
    connection_id: &str,
    command: String,
    args: String,
    elapsed_ms: u128,
    status: &str,
) {
    let connection_name = state
        .sessions
        .lock()
        .await
        .get(connection_id)
        .map(|session| session.connection_name.clone())
        .unwrap_or_else(|| connection_id.to_string());
    let _ = app.emit(
        "redis://command-log",
        CommandLogEvent {
            connection_id: connection_id.to_string(),
            connection_name,
            command,
            args,
            elapsed_ms,
            status: status.to_string(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{connection_status_event, summarize_cli_for_log};

    #[test]
    fn summarizes_cli_args_for_command_log() {
        let (command, args) = summarize_cli_for_log("AUTH 'secret'");

        assert_eq!(command, "AUTH");
        assert_eq!(args, "***");
    }

    #[test]
    fn builds_connection_status_event_without_secret_fields() {
        let event = connection_status_event(
            Some("session-1"),
            Some("stored-1"),
            "Production Redis",
            "failed",
            Some("redis.connection_failed"),
            Some("Connection failed"),
        );
        let serialized = serde_json::to_value(&event).unwrap();

        assert_eq!(event.connection_id.as_deref(), Some("session-1"));
        assert_eq!(event.connection_key.as_deref(), Some("stored-1"));
        assert_eq!(event.connection_name, "Production Redis");
        assert_eq!(event.status, "failed");
        assert_eq!(event.code.as_deref(), Some("redis.connection_failed"));
        assert_eq!(event.message.as_deref(), Some("Connection failed"));
        assert!(serialized.get("connectionId").is_some());
        assert!(serialized.get("connectionKey").is_some());
        assert!(serialized.get("connection_id").is_none());
        assert!(!serialized.to_string().contains("redis-secret"));
    }
}
