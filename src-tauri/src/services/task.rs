use crate::error::{AppError, AppResult};
use crate::models::{
    KeyEntry, TaskEventItem, TaskProgressEvent, TaskStatus, TaskStatusSnapshot, TtlCondition,
    TtlMode,
};
use crate::services::capabilities::{self, CapabilityId};
use crate::services::executor::{PipelineTarget, RedisExecutor};
use crate::services::{cluster, connection, redis_values, values};
use crate::state::{AppState, RedisExecutorHandle, RedisSessionRuntimeHandle, TaskRecord};
use base64::Engine;
use futures_util::StreamExt;
use redis::FromRedisValue;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};

pub type TaskCancelToken = Arc<AtomicBool>;

/// 暂停标志，与 `TaskCancelToken` 同一模式：注册表持有一份，任务循环轮询。
/// 为什么它不是一个 `TaskStatus`，见 `set_task_paused`。
pub type TaskPauseFlag = Arc<AtomicBool>;

/// 扫描任务批间 pacing 的上限：页间睡「上一页的实测耗时」，封顶在此值。
///
/// 串行 SCAN 对服务端的占用上界是 t_scan / (t_scan + RTT)：每页命令之间隔着
/// 一个完整往返，Redis 在间隙里服务别人。WAN 上 RTT 本身就是几十上百毫秒，
/// 占空比天然低；**本机连生产实例时 RTT 近乎为零**，全速循环会把占空比推向
/// 100%。睡等量于上一页的实测耗时把占空比压回 ~50%，且等待随实际开销缩放：
/// 曾经的固定 40ms 按「百万 key 多等 20 秒」论证，到千万级库就线性放大成
/// 主导成本（3356 万 key ≈ 1.7 万页 × 40ms ≈ 11 分钟纯睡眠，扫描本身
/// 不到 1 分钟）——搜索是用户盯着等的前台路径，等不起按最坏假设付费。
/// 封顶 40ms 是给 WAN 的：页耗时被 RTT 主导时占空比本来就低，
/// 不该再对着 RTT 翻倍等待。
/// 游标已归零（最后一页）不睡 —— 任务该立刻完成。
/// 见 `doc/backend/tasks.md`「节流：按占空比自适应」。
const SCAN_TASK_PACING_CAP: Duration = Duration::from_millis(40);

/// 暂停等待的轮询间隔。与 Monitor / 订阅循环里已有的 100ms sleep 同一量级：
/// 恢复延迟人察觉不到，空转开销可忽略；等待期间取消照常即时生效。
const TASK_PAUSE_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MonitorSetupDecision {
    Cancelled,
    Failed,
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportWriteDecision {
    Cancelled,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CommandImportLineAction {
    Skip,
    Execute(String),
}

fn monitor_setup_decision<T, E>(
    result: &Result<T, E>,
    cancel: &TaskCancelToken,
) -> MonitorSetupDecision {
    if cancel.load(Ordering::SeqCst) {
        MonitorSetupDecision::Cancelled
    } else if result.is_err() {
        MonitorSetupDecision::Failed
    } else {
        MonitorSetupDecision::Continue
    }
}

fn should_cancel_export_before_write(cancel: &TaskCancelToken) -> ExportWriteDecision {
    if cancel.load(Ordering::SeqCst) {
        ExportWriteDecision::Cancelled
    } else {
        ExportWriteDecision::Write
    }
}

fn command_import_line_action(line: &str) -> CommandImportLineAction {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        CommandImportLineAction::Skip
    } else {
        CommandImportLineAction::Execute(trimmed.to_string())
    }
}

fn command_import_failure_summary(line_number: u64, error: &AppError) -> String {
    format!("line {line_number}: {}", error.payload().message)
}

fn command_import_completed_message(
    failed: u64,
    first_failure_summary: Option<&str>,
) -> Option<String> {
    if failed == 0 {
        return None;
    }
    let mut message = format!("Imported commands with {failed} failed commands");
    if let Some(summary) = first_failure_summary.filter(|summary| !summary.is_empty()) {
        message.push_str(": ");
        message.push_str(summary);
    }
    Some(message)
}

pub async fn require_connection_name(state: &AppState, connection_id: &str) -> AppResult<String> {
    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(connection_id)
        .ok_or_else(|| AppError::ConnectionNotFound(connection_id.into()))?;
    Ok(session.connection_name.clone())
}

async fn require_session_runtime(
    state: &AppState,
    connection_id: &str,
) -> AppResult<RedisSessionRuntimeHandle> {
    let sessions = state.sessions.lock().await;
    sessions
        .get(connection_id)
        .map(|session| session.runtime.clone())
        .ok_or_else(|| AppError::ConnectionNotFound(connection_id.into()))
}

async fn ensure_session_writable(
    state: &AppState,
    connection_id: &str,
    command: &str,
) -> AppResult<()> {
    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(connection_id)
        .ok_or_else(|| AppError::ConnectionNotFound(connection_id.into()))?;
    if session.readonly {
        return Err(AppError::ReadonlyWriteRejected(command.into()));
    }
    Ok(())
}

fn emit_task_event(app: &AppHandle, event: TaskProgressEvent) {
    let _ = app.emit("redis://task-progress", event);
}

pub async fn start_load_all_keys_task(
    app: AppHandle,
    state: &AppState,
    connection_id: String,
    options: crate::models::KeyScanTaskOptions,
) -> AppResult<crate::models::TaskStarted> {
    start_scan_task(app, state, connection_id, "load_all_keys", options).await
}

pub async fn start_search_keys_task(
    app: AppHandle,
    state: &AppState,
    connection_id: String,
    options: crate::models::KeyScanTaskOptions,
) -> AppResult<crate::models::TaskStarted> {
    start_scan_task(app, state, connection_id, "search_keys", options).await
}

pub async fn start_memory_analysis_task(
    app: AppHandle,
    state: &AppState,
    connection_id: String,
    options: crate::models::MemoryAnalysisTaskOptions,
) -> AppResult<crate::models::TaskStarted> {
    let connection_name = require_connection_name(state, &connection_id).await?;
    let snapshot = register_task(
        state,
        connection_id.clone(),
        connection_name,
        "memory_analysis".into(),
    )
    .await;
    let task_id = snapshot.task_id.clone();
    let cancel = task_cancel_token(state, &task_id).await?;
    emit_task_event(&app, progress_event(&snapshot, vec![], None));

    let state = state.clone_for_task();
    let pattern = normalize_scan_pattern(options.pattern);
    let count = normalize_scan_count(options.count);
    let min_size = options.min_size;
    let max_results = options.max_results.unwrap_or(200_000);
    let task_id_for_task = task_id.clone();
    tauri::async_runtime::spawn(async move {
        run_memory_analysis_task(MemoryAnalysisRun {
            app,
            state,
            task_id: task_id_for_task,
            connection_id,
            pattern,
            count,
            min_size,
            max_results,
            cancel,
        })
        .await;
    });

    Ok(crate::models::TaskStarted { task_id })
}

pub async fn start_batch_delete_task(
    app: AppHandle,
    state: &AppState,
    connection_id: String,
    options: crate::models::BatchDeleteTaskOptions,
) -> AppResult<crate::models::TaskStarted> {
    let dry_run = options.dry_run.unwrap_or(false);

    let connection_name = {
        let sessions = state.sessions.lock().await;
        let session = sessions
            .get(&connection_id)
            .ok_or_else(|| AppError::ConnectionNotFound(connection_id.clone()))?;
        // 试算不删任何东西，只读会话上照样能跑 —— 「我不敢直接删，先看看有多少个」
        // 正是只读连接上会做的事。拒绝的是写，不是这个任务。与批量 TTL 一致。
        if session.readonly && !dry_run {
            return Err(AppError::ReadonlyWriteRejected("DEL".into()));
        }
        session.connection_name.clone()
    };

    let explicit_keys = options
        .keys
        .iter()
        .map(redis_arg_to_string)
        .collect::<AppResult<Vec<_>>>()?;
    let patterns = options
        .patterns
        .iter()
        .filter_map(|pattern| batch_scan_pattern(pattern))
        .collect::<Vec<_>>();

    if explicit_keys.is_empty() && patterns.is_empty() {
        return Err(AppError::TaskInvalidRequest(
            "Batch delete requires keys or patterns".into(),
        ));
    }

    let snapshot = register_task(
        state,
        connection_id.clone(),
        connection_name,
        batch_task_type("batch_delete", dry_run),
    )
    .await;
    let task_id = snapshot.task_id.clone();
    let cancel = task_cancel_token(state, &task_id).await?;
    emit_task_event(&app, progress_event(&snapshot, vec![], None));

    let state = state.clone_for_task();
    let task_id_for_task = task_id.clone();
    let scan_count = normalize_scan_count(options.count);
    tauri::async_runtime::spawn(async move {
        run_batch_delete_task(BatchDeleteRun {
            app,
            state,
            task_id: task_id_for_task,
            connection_id,
            explicit_keys,
            patterns,
            dry_run,
            scan_count,
            cancel,
        })
        .await;
    });

    Ok(crate::models::TaskStarted { task_id })
}

pub async fn start_batch_ttl_task(
    app: AppHandle,
    state: &AppState,
    connection_id: String,
    options: crate::models::BatchTtlTaskOptions,
) -> AppResult<crate::models::TaskStarted> {
    let dry_run = options.dry_run.unwrap_or(false);
    let spec = TtlSpec {
        mode: options.mode,
        value: options.value,
        condition: options.condition,
    };
    validate_ttl_spec(&spec, current_unix_seconds())?;

    let connection_name = {
        let sessions = state.sessions.lock().await;
        let session = sessions
            .get(&connection_id)
            .ok_or_else(|| AppError::ConnectionNotFound(connection_id.clone()))?;
        // 试算不写任何东西，只读会话上照样能跑 —— 「我不敢直接改，先看看影响多少」
        // 正是只读连接上会做的事。拒绝的是写，不是这个任务。
        if session.readonly && !dry_run {
            return Err(AppError::ReadonlyWriteRejected(
                values::ttl_command_name(spec.mode).into(),
            ));
        }
        // 条件区在界面上按同一项能力置灰，这里是后端一侧的同一条规则。
        // 试算也拦：带着一个服务端不认的条件去试算，量出来的数字对应不上任何真实执行。
        if spec.condition.is_some() {
            capabilities::require(&session.runtime, CapabilityId::ExpireConditions)?;
        }
        session.connection_name.clone()
    };

    let explicit_keys = options
        .keys
        .iter()
        .map(redis_arg_to_string)
        .collect::<AppResult<Vec<_>>>()?;
    let patterns = options
        .patterns
        .iter()
        .filter_map(|pattern| batch_scan_pattern(pattern))
        .collect::<Vec<_>>();

    if explicit_keys.is_empty() && patterns.is_empty() {
        return Err(AppError::TaskInvalidRequest(
            "Batch TTL requires keys or patterns".into(),
        ));
    }

    let snapshot = register_task(
        state,
        connection_id.clone(),
        connection_name,
        batch_task_type("batch_ttl", dry_run),
    )
    .await;
    let task_id = snapshot.task_id.clone();
    let cancel = task_cancel_token(state, &task_id).await?;
    emit_task_event(&app, progress_event(&snapshot, vec![], None));

    let state = state.clone_for_task();
    let task_id_for_task = task_id.clone();
    let scan_count = normalize_scan_count(options.count);
    tauri::async_runtime::spawn(async move {
        run_batch_ttl_task(BatchTtlRun {
            app,
            state,
            task_id: task_id_for_task,
            connection_id,
            explicit_keys,
            patterns,
            spec,
            dry_run,
            scan_count,
            cancel,
        })
        .await;
    });

    Ok(crate::models::TaskStarted { task_id })
}

pub async fn start_import_keys_task(
    app: AppHandle,
    state: &AppState,
    connection_id: String,
    file_path: String,
) -> AppResult<crate::models::TaskStarted> {
    ensure_session_writable(state, &connection_id, "RESTORE").await?;
    let connection_name = require_connection_name(state, &connection_id).await?;
    let snapshot = register_task(
        state,
        connection_id.clone(),
        connection_name,
        "import_keys".into(),
    )
    .await;
    let task_id = snapshot.task_id.clone();
    let cancel = task_cancel_token(state, &task_id).await?;
    emit_task_event(
        &app,
        progress_event(
            &snapshot,
            vec![TaskEventItem::File {
                path: file_path.clone(),
            }],
            None,
        ),
    );
    let state = state.clone_for_task();
    let task_id_for_task = task_id.clone();
    tauri::async_runtime::spawn(async move {
        run_import_keys_task(
            app,
            state,
            task_id_for_task,
            connection_id,
            file_path,
            cancel,
        )
        .await;
    });
    Ok(crate::models::TaskStarted { task_id })
}

pub async fn start_import_commands_task(
    app: AppHandle,
    state: &AppState,
    connection_id: String,
    file_path: String,
) -> AppResult<crate::models::TaskStarted> {
    ensure_session_writable(state, &connection_id, "COMMAND FILE IMPORT").await?;
    let connection_name = require_connection_name(state, &connection_id).await?;
    let snapshot = register_task(
        state,
        connection_id.clone(),
        connection_name,
        "import_commands".into(),
    )
    .await;
    let task_id = snapshot.task_id.clone();
    let cancel = task_cancel_token(state, &task_id).await?;
    emit_task_event(
        &app,
        progress_event(
            &snapshot,
            vec![TaskEventItem::File {
                path: file_path.clone(),
            }],
            None,
        ),
    );
    let state = state.clone_for_task();
    let task_id_for_task = task_id.clone();
    tauri::async_runtime::spawn(async move {
        run_import_commands_task(
            app,
            state,
            task_id_for_task,
            connection_id,
            file_path,
            cancel,
        )
        .await;
    });
    Ok(crate::models::TaskStarted { task_id })
}

pub async fn start_export_keys_task(
    app: AppHandle,
    state: &AppState,
    connection_id: String,
    options: crate::models::ExportKeysTaskOptions,
) -> AppResult<crate::models::TaskStarted> {
    let explicit_keys = options
        .keys
        .iter()
        .map(redis_arg_to_bytes)
        .collect::<AppResult<Vec<_>>>()?;
    if explicit_keys.is_empty() {
        return Err(AppError::TaskInvalidRequest(
            "Export requires at least one key".into(),
        ));
    }
    let connection_name = require_connection_name(state, &connection_id).await?;
    let snapshot = register_task(
        state,
        connection_id.clone(),
        connection_name,
        "export_keys".into(),
    )
    .await;
    let task_id = snapshot.task_id.clone();
    let cancel = task_cancel_token(state, &task_id).await?;
    emit_task_event(
        &app,
        progress_event(
            &snapshot,
            vec![TaskEventItem::File {
                path: options.output_path.clone(),
            }],
            None,
        ),
    );
    let state = state.clone_for_task();
    let task_id_for_task = task_id.clone();
    tauri::async_runtime::spawn(async move {
        run_export_keys_task(
            app,
            state,
            task_id_for_task,
            connection_id,
            explicit_keys,
            options.output_path,
            cancel,
        )
        .await;
    });
    Ok(crate::models::TaskStarted { task_id })
}

pub async fn start_monitor_task(
    app: AppHandle,
    state: &AppState,
    connection_id: String,
) -> AppResult<crate::models::TaskStarted> {
    let config = connection::session_config(state, &connection_id).await?;
    let connection_name = require_connection_name(state, &connection_id).await?;
    let snapshot = register_task(
        state,
        connection_id.clone(),
        connection_name,
        "monitor".into(),
    )
    .await;
    let task_id = snapshot.task_id.clone();
    let cancel = task_cancel_token(state, &task_id).await?;
    emit_task_event(&app, progress_event(&snapshot, vec![], None));

    let state = state.clone_for_task();
    let task_id_for_task = task_id.clone();
    tauri::async_runtime::spawn(async move {
        run_monitor_task(app, state, task_id_for_task, config, cancel).await;
    });

    Ok(crate::models::TaskStarted { task_id })
}

pub async fn start_subscription_task(
    app: AppHandle,
    state: &AppState,
    connection_id: String,
    options: crate::models::SubscriptionTaskOptions,
) -> AppResult<crate::models::TaskStarted> {
    let (kind, values) = normalize_subscription_options(&options)?;
    let config = connection::session_config(state, &connection_id).await?;
    let connection_name = require_connection_name(state, &connection_id).await?;
    let task_type = match kind {
        crate::models::SubscriptionKind::Subscribe => "subscribe",
        crate::models::SubscriptionKind::Psubscribe => "psubscribe",
    };
    let snapshot = register_task(
        state,
        connection_id.clone(),
        connection_name,
        task_type.into(),
    )
    .await;
    let task_id = snapshot.task_id.clone();
    let cancel = task_cancel_token(state, &task_id).await?;
    emit_task_event(&app, progress_event(&snapshot, vec![], None));

    let state = state.clone_for_task();
    let task_id_for_task = task_id.clone();
    tauri::async_runtime::spawn(async move {
        run_subscription_task(app, state, task_id_for_task, config, kind, values, cancel).await;
    });

    Ok(crate::models::TaskStarted { task_id })
}

async fn start_scan_task(
    app: AppHandle,
    state: &AppState,
    connection_id: String,
    task_type: &str,
    options: crate::models::KeyScanTaskOptions,
) -> AppResult<crate::models::TaskStarted> {
    let connection_name = require_connection_name(state, &connection_id).await?;
    let snapshot = register_task(
        state,
        connection_id.clone(),
        connection_name,
        task_type.into(),
    )
    .await;
    let task_id = snapshot.task_id.clone();
    let cancel = task_cancel_token(state, &task_id).await?;
    let paused = task_pause_flag(state, &task_id).await?;
    emit_task_event(&app, progress_event(&snapshot, vec![], None));

    let state = state.clone_for_task();
    let pattern = normalize_scan_pattern(options.pattern);
    let count = normalize_scan_count(options.count);
    let with_ttl = options.with_ttl.unwrap_or(false);
    let task_id_for_task = task_id.clone();
    tauri::async_runtime::spawn(async move {
        run_scan_task(ScanRun {
            app,
            state,
            task_id: task_id_for_task,
            connection_id,
            pattern,
            count,
            with_ttl,
            cancel,
            paused,
        })
        .await;
    });

    Ok(crate::models::TaskStarted { task_id })
}

struct ScanRun {
    app: AppHandle,
    state: AppState,
    task_id: String,
    connection_id: String,
    pattern: Option<String>,
    count: usize,
    with_ttl: bool,
    cancel: TaskCancelToken,
    paused: TaskPauseFlag,
}

async fn run_scan_task(run: ScanRun) {
    let ScanRun {
        app,
        state,
        task_id,
        connection_id,
        pattern,
        count,
        with_ttl,
        cancel,
        paused,
    } = run;
    let mut cursor = "0".to_string();
    let mut processed = 0_u64;
    let mut succeeded = 0_u64;
    // 单机估算器零网络交互，首页持锁间隙顺手建；集群的要逐节点采集
    // DBSIZE 权重（按节点 DBSIZE 加权聚合，见 `doc/backend/tasks.md`「扫描任务：只报命中数」），
    // 挪到首页事件之后、executor 锁外短持锁进行 —— 晚一页就位可接受，
    // 就位前 estimated_progress 为 None 本来就是合法状态。
    let mut estimator: Option<ScanProgressEstimator> = None;

    loop {
        if cancel.load(Ordering::SeqCst) {
            if let Ok(snapshot) = cancel_task(&state, &task_id).await {
                emit_task_event(&app, progress_event(&snapshot, vec![], None));
            }
            return;
        }

        // 窗口隐藏时前端把任务暂停（`redis_set_task_paused`），恢复后从当前
        // 游标继续。等待发生在页间、executor 锁之外 —— 暂停中的任务不挡
        // 交互命令。等待期间的取消由紧随其后的检查兜住。
        wait_while_paused(&paused, &cancel).await;
        if cancel.load(Ordering::SeqCst) {
            if let Ok(snapshot) = cancel_task(&state, &task_id).await {
                emit_task_event(&app, progress_event(&snapshot, vec![], None));
            }
            return;
        }

        let runtime = match require_session_runtime(&state, &connection_id).await {
            Ok(runtime) => runtime,
            Err(_) => {
                if let Ok(event) =
                    mark_task_failed_event(&state, &task_id, "Connection is closed".into()).await
                {
                    emit_task_event(&app, event);
                }
                return;
            }
        };

        let (page, page_cost) = {
            let mut executor = runtime.executor.lock().await;
            // is_cluster 只看枚举变体，不发命令；集群估算器的采集有真 RTT，
            // 绝不放进这次持锁交互（见下面 pacing 分支）。
            if estimator.is_none() && !executor.is_cluster() {
                estimator = Some(ScanProgressEstimator::standalone());
            }
            // 计时在锁内起 —— 锁等待是交互命令在用连接，不是服务端为这一页
            // 付出的开销，计进去会放大 pacing。
            let started = Instant::now();
            let page = scan_and_annotate_one_page(
                &mut executor,
                &cursor,
                pattern.as_deref(),
                count,
                with_ttl,
            )
            .await;
            (page, started.elapsed())
        };

        if cancel.load(Ordering::SeqCst) {
            if let Ok(snapshot) = cancel_task(&state, &task_id).await {
                emit_task_event(&app, progress_event(&snapshot, vec![], None));
            }
            return;
        }

        let (next_cursor, entries) = match page {
            Ok(page) => page,
            Err(error) => {
                if let Ok(event) =
                    mark_task_failed_event(&state, &task_id, error.payload().message).await
                {
                    emit_task_event(&app, event);
                }
                return;
            }
        };

        processed += entries.len() as u64;
        succeeded += entries.len() as u64;
        // 近似进度从**返回的**游标算：首个非零游标即暴露表的位宽。
        let estimated_progress = estimator
            .as_mut()
            .and_then(|estimator| estimator.observe(&next_cursor));
        // 空页也发：MATCH 稀疏时整页可能一个不中，此时只有估算在前进 ——
        // 不发事件，前端的「约 P%」就停在上一页。页间 pacing 等量于页耗时，
        // 本机全空页的最坏情况也就几百条/秒，仍低于 Monitor 的千条级，
        // 这个频道扛得住。
        let _ = emit_task_key_progress_event(
            &app,
            &state,
            &task_id,
            KeyProgressUpdate {
                processed,
                succeeded,
                failed: 0,
                total: None,
                estimated_progress,
            },
            entries,
        )
        .await;

        cursor = next_cursor;
        match scan_pacing_delay(&cursor, page_cost) {
            // 游标归零 = 最后一页：立刻完成，不再 pacing。
            None => {
                if let Ok(snapshot) = mark_task_completed(&state, &task_id).await {
                    emit_task_event(&app, progress_event(&snapshot, vec![], None));
                }
                return;
            }
            Some(delay) => {
                // 集群估算器在首页事件之后、页间隙就位：`for_executor` 内部
                // 逐节点短持锁采集 DBSIZE。曾经放在首页扫描的同一次持锁
                // 交互里串行 N 个 RTT —— 大集群/慢节点下同会话的交互命令
                // 会被挡住整段采集时长。
                if estimator.is_none() {
                    estimator = Some(ScanProgressEstimator::for_executor(&runtime.executor).await);
                }
                tokio::time::sleep(delay).await
            }
        }
    }
}

/// 下一页之前该不该睡、睡多久。`None` 表示游标已归零（最后一页）——
/// 不睡，任务立刻完成。占空比与封顶的论证见 `SCAN_TASK_PACING_CAP`。
fn scan_pacing_delay(next_cursor: &str, page_cost: Duration) -> Option<Duration> {
    (next_cursor != "0").then_some(page_cost.min(SCAN_TASK_PACING_CAP))
}

/// 暂停中就等，恢复或取消就返回。只在扫描循环的页间调用 ——
/// 不持有 executor 锁，暂停不影响同会话的交互命令。
async fn wait_while_paused(paused: &TaskPauseFlag, cancel: &TaskCancelToken) {
    while should_keep_waiting_while_paused(paused, cancel) {
        tokio::time::sleep(TASK_PAUSE_POLL_INTERVAL).await;
    }
}

fn should_keep_waiting_while_paused(paused: &TaskPauseFlag, cancel: &TaskCancelToken) -> bool {
    paused.load(Ordering::SeqCst) && !cancel.load(Ordering::SeqCst)
}

/* ── 扫描进度的近似估算（doc/backend/tasks.md「扫描任务：只报命中数」）─────────────────
SCAN 游标是反转二进制迭代：把游标的低 `bits` 位反转就得到一个随扫描
单调递增的迭代序号，`rev_bits(cursor) / 2^bits` 即近似进度。表的位宽
不用问服务端 —— 首个非零游标的位宽就是它（首批返回的游标 ≈ 表大小/2），
rehash 扩表时观测到更宽的游标再抬高。
已知的失真来源（正因如此显示必须带「约」）：rehash 期间游标序列抖动、
大批删除后表大小与 DBSIZE 脱节、集群加权用的 DBSIZE 只在任务开始时
取一次。对外估算值做单调不减约束（任务内高水位），抖动不外泄。 */

/// 扫描任务的近似进度估算器。`pub` 的理由与 `scan_one_page` 相同：
/// 任务启动入口要 `AppHandle`，e2e 里没有，真正消费游标的这一层单独
/// 暴露出来供 e2e 直接驱动（断言估算单调不减）。
pub struct ScanProgressEstimator {
    kind: ScanEstimatorKind,
    /// 对外的单调高水位：位宽抬高让分母翻倍、集群节点间的推进不均，
    /// 都可能让原始估算回落，这里只允许不减。
    high_water: Option<f64>,
}

enum ScanEstimatorKind {
    Standalone(NodeScanEstimator),
    Cluster(ClusterScanEstimator),
}

/// 单个节点（或单机实例）的游标估算。
#[derive(Default)]
struct NodeScanEstimator {
    /// 观测到的最大游标位宽；0 = 尚无信息。
    bits: u32,
    /// 节点内高水位。
    fraction: Option<f64>,
}

impl NodeScanEstimator {
    /// 喂一个观测到的**下一页游标**。返回 `None` 仅当从未观测成功过
    /// （首页之前 / 游标解析不出来）；游标归零 = 该节点扫完 = 1.0。
    fn observe(&mut self, cursor: &str) -> Option<f64> {
        let next = if cursor == "0" {
            Some(1.0)
        } else {
            cursor
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .map(|value| {
                    self.bits = self.bits.max(64 - value.leading_zeros());
                    reversed_cursor_fraction(value, self.bits)
                })
        };
        if let Some(next) = next {
            self.fraction = Some(self.fraction.map_or(next, |current| current.max(next)));
        }
        self.fraction
    }
}

/// `rev_bits(cursor) / 2^bits`。调用方保证 `bits >= 游标的位宽 >= 1`。
fn reversed_cursor_fraction(cursor: u64, bits: u32) -> f64 {
    let reversed = cursor.reverse_bits() >> (64 - bits);
    (reversed as f64 / 2f64.powi(bits as i32)).min(1.0)
}

/// 集群：对合成游标里的每个节点独立估算，按节点 DBSIZE 加权聚合。
struct ClusterScanEstimator {
    /// 任务启动时的主节点端点（`host:port`）。合成游标里**缺席**的端点
    /// 意味着该节点游标已归零（`encode_cluster_scan_cursor` 只保留非零
    /// 游标）—— 已扫完，计 100%。
    endpoints: Vec<String>,
    /// 端点 → DBSIZE 权重；`None` = 等权（取权重失败的退化，见
    /// `cluster_estimator_inputs`）。
    weights: Option<HashMap<String, f64>>,
    nodes: HashMap<String, NodeScanEstimator>,
}

impl ClusterScanEstimator {
    fn observe(&mut self, cursor: &str) -> Option<f64> {
        if cursor == "0" {
            return Some(1.0);
        }
        // 编解码只有一处实现（connection.rs，`scan_one_page` 的注释原话）。
        // masters 参数只在游标为 "0" 的引导页用到，上面已排除，传空即可。
        let Ok(remaining) = connection::decode_cluster_scan_cursor(cursor, &[]) else {
            return None;
        };
        // 拓扑漂移（resharding 加主节点）时合成游标里可能出现启动时
        // 没见过的端点，补进分母，别让它凭空拉高进度。
        let mut endpoints: Vec<&str> = self.endpoints.iter().map(String::as_str).collect();
        for endpoint in remaining.keys() {
            if !self.endpoints.iter().any(|known| known == endpoint) {
                endpoints.push(endpoint);
            }
        }
        if endpoints.is_empty() {
            return None;
        }

        let mut weighted = 0_f64;
        let mut total_weight = 0_f64;
        for endpoint in endpoints {
            let fraction = match remaining.get(endpoint) {
                // 缺席 = 游标已归零 = 该节点已扫完
                None => 1.0,
                Some(node_cursor) => self
                    .nodes
                    .entry(endpoint.to_string())
                    .or_default()
                    .observe(node_cursor)
                    .unwrap_or(0.0),
            };
            let weight = self.weight_of(endpoint);
            weighted += fraction * weight;
            total_weight += weight;
        }
        (total_weight > 0.0).then(|| weighted / total_weight)
    }

    fn weight_of(&self, endpoint: &str) -> f64 {
        let Some(weights) = &self.weights else {
            return 1.0;
        };
        weights.get(endpoint).copied().unwrap_or_else(|| {
            // 启动后才出现的节点没有权重：按已知权重的均值计 ——
            // 比 0（无视它）或 1（几乎无视它）都更接近真相。
            if weights.is_empty() {
                1.0
            } else {
                weights.values().sum::<f64>() / weights.len() as f64
            }
        })
    }
}

impl ScanProgressEstimator {
    pub fn standalone() -> Self {
        Self {
            kind: ScanEstimatorKind::Standalone(NodeScanEstimator::default()),
            high_water: None,
        }
    }

    fn cluster(endpoints: Vec<String>, weights: Option<HashMap<String, f64>>) -> Self {
        Self {
            kind: ScanEstimatorKind::Cluster(ClusterScanEstimator {
                endpoints,
                weights,
                nodes: HashMap::new(),
            }),
            high_water: None,
        }
    }

    /// 按 executor 的形态建估算器。集群下要发一轮每节点 DBSIZE，每条命令
    /// 各自短持锁（见 `cluster_estimator_inputs`）—— 调用方**不要**在已经
    /// 持有 executor 锁时调用，否则第一次内部加锁就死锁。
    pub async fn for_executor(executor: &RedisExecutorHandle) -> Self {
        let is_cluster = executor.lock().await.is_cluster();
        if !is_cluster {
            return Self::standalone();
        }
        let (endpoints, weights) = cluster_estimator_inputs(executor).await;
        Self::cluster(endpoints, weights)
    }

    /// 喂一个 `scan_and_annotate_one_page` 返回的游标（单机裸游标或
    /// `cluster:{json}` 合成游标），返回单调不减的近似进度（0..=1）。
    /// `None` = 尚无信息（估算不可用，前端不显示百分比）。
    pub fn observe(&mut self, next_cursor: &str) -> Option<f64> {
        let raw = match &mut self.kind {
            ScanEstimatorKind::Standalone(node) => node.observe(next_cursor),
            ScanEstimatorKind::Cluster(cluster) => cluster.observe(next_cursor),
        };
        if let Some(raw) = raw {
            let clamped = raw.clamp(0.0, 1.0);
            self.high_water = Some(self.high_water.map_or(clamped, |hw| hw.max(clamped)));
        }
        self.high_water
    }
}

/// 集群估算的准备工作：主节点列表 + 每节点一条 DBSIZE 作权重。
/// 任务开始时取一次就够 —— 估算本来就是近似的，不追着 keyspace 变化跑。
///
/// **不长持 executor 锁**：CLUSTER NODES 与每节点 DBSIZE 各自短持锁发一条，
/// 间隙里同会话的交互命令照常插队 —— 把 N 个 RTT 串进一次持锁交互，
/// 大集群/慢节点下会话会卡住整段采集时长。代价是采集间隙 keyspace 可能
/// 变动，但估算本来就是近似的（显示带「约」），一致性不值得拿卡顿换。
///
/// **任一节点的 DBSIZE 取不到（ACL / rename-command）就整体退化为等权**：
/// 槽位默认均匀分布、节点体量通常同量级，等权仍是单调且量级正确的估算，
/// 为一个权限细节丢掉整条进度不值；而真权重与凭空凑的权重混在一起
/// 比全部等权更歪，所以是整体退化而不是逐节点补缺。全空库（权重和为 0）
/// 同理退化 —— 加权平均的分母不能是 0。
async fn cluster_estimator_inputs(
    executor: &RedisExecutorHandle,
) -> (Vec<String>, Option<HashMap<String, f64>>) {
    let masters = {
        let mut executor = executor.lock().await;
        match connection::query_cluster_nodes(&mut executor).await {
            Ok(nodes) => cluster::parse_master_nodes(&nodes).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    };
    let endpoints: Vec<String> = masters.iter().map(|master| master.endpoint()).collect();

    let mut weights = HashMap::new();
    for master in &masters {
        let size = {
            let mut executor = executor.lock().await;
            match executor
                .query_cluster_node_raw(redis::cmd("DBSIZE"), master.host.clone(), master.port)
                .await
            {
                Ok(value) => i64::from_redis_value(value).ok(),
                Err(_) => None,
            }
        };
        match size {
            Some(size) if size >= 0 => {
                weights.insert(master.endpoint(), size as f64);
            }
            _ => return (endpoints, None),
        }
    }
    if endpoints.is_empty() || weights.values().sum::<f64>() <= 0.0 {
        (endpoints, None)
    } else {
        (endpoints, Some(weights))
    }
}

async fn run_monitor_task(
    app: AppHandle,
    state: AppState,
    task_id: String,
    config: crate::models::ConnectionConfig,
    cancel: TaskCancelToken,
) {
    if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
        return;
    }

    let dedicated_result = connection::open_dedicated_client(&config).await;
    match monitor_setup_decision(&dedicated_result, &cancel) {
        MonitorSetupDecision::Cancelled => {
            let _ = emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await;
            return;
        }
        MonitorSetupDecision::Failed => {
            if let Err(error) = dedicated_result {
                emit_failed_for_error(&app, &state, &task_id, &error).await;
            }
            return;
        }
        MonitorSetupDecision::Continue => {}
    }
    let dedicated = match dedicated_result {
        Ok(dedicated) => dedicated,
        Err(_) => return,
    };

    let monitor_result = dedicated.client.get_async_monitor().await;
    match monitor_setup_decision(&monitor_result, &cancel) {
        MonitorSetupDecision::Cancelled => {
            let _ = emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await;
            return;
        }
        MonitorSetupDecision::Failed => {
            if let Err(error) = monitor_result {
                emit_failed_for_error(
                    &app,
                    &state,
                    &task_id,
                    &AppError::RedisConnectionFailed(error.to_string()),
                )
                .await;
            }
            return;
        }
        MonitorSetupDecision::Continue => {}
    }
    let monitor = match monitor_result {
        Ok(monitor) => monitor,
        Err(_) => return,
    };
    let _resources = dedicated.resources;
    let mut stream = monitor.into_on_message::<String>();
    let mut processed = 0_u64;
    let mut succeeded = 0_u64;

    loop {
        if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
            return;
        }

        tokio::select! {
            line = stream.next() => {
                match line {
                    Some(raw) => {
                        if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
                            return;
                        }
                        processed += 1;
                        succeeded += 1;
                        if let Ok(snapshot) = mark_task_progress(
                            &state,
                            &task_id,
                            processed,
                            succeeded,
                            0,
                            None,
                        )
                        .await
                        {
                            if let Some(event) =
                                monitor_progress_event_for_snapshot(&snapshot, &raw)
                            {
                                emit_task_event(&app, event);
                            }
                        }
                    }
                    None => {
                        if let Ok(snapshot) = mark_task_completed(&state, &task_id).await {
                            emit_task_event(
                                &app,
                                progress_event(
                                    &snapshot,
                                    vec![],
                                    Some("Monitor connection closed".into()),
                                ),
                            );
                        }
                        return;
                    }
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
    }
}

async fn run_subscription_task(
    app: AppHandle,
    state: AppState,
    task_id: String,
    config: crate::models::ConnectionConfig,
    kind: crate::models::SubscriptionKind,
    values: Vec<String>,
    cancel: TaskCancelToken,
) {
    if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
        return;
    }

    let dedicated_result = connection::open_dedicated_client(&config).await;
    match monitor_setup_decision(&dedicated_result, &cancel) {
        MonitorSetupDecision::Cancelled => {
            let _ = emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await;
            return;
        }
        MonitorSetupDecision::Failed => {
            if let Err(error) = dedicated_result {
                emit_failed_for_error(&app, &state, &task_id, &error).await;
            }
            return;
        }
        MonitorSetupDecision::Continue => {}
    }
    let dedicated = match dedicated_result {
        Ok(dedicated) => dedicated,
        Err(_) => return,
    };

    let pubsub_result = dedicated.client.get_async_pubsub().await;
    match monitor_setup_decision(&pubsub_result, &cancel) {
        MonitorSetupDecision::Cancelled => {
            let _ = emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await;
            return;
        }
        MonitorSetupDecision::Failed => {
            if let Err(error) = pubsub_result {
                emit_failed_for_error(
                    &app,
                    &state,
                    &task_id,
                    &AppError::RedisConnectionFailed(error.to_string()),
                )
                .await;
            }
            return;
        }
        MonitorSetupDecision::Continue => {}
    }
    let mut pubsub = match pubsub_result {
        Ok(pubsub) => pubsub,
        Err(_) => return,
    };

    let subscribe_result = match kind {
        crate::models::SubscriptionKind::Subscribe => pubsub.subscribe(values.as_slice()).await,
        crate::models::SubscriptionKind::Psubscribe => pubsub.psubscribe(values.as_slice()).await,
    };
    match monitor_setup_decision(&subscribe_result, &cancel) {
        MonitorSetupDecision::Cancelled => {
            let _ = emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await;
            return;
        }
        MonitorSetupDecision::Failed => {
            if let Err(error) = subscribe_result {
                emit_failed_for_error(
                    &app,
                    &state,
                    &task_id,
                    &AppError::RedisConnectionFailed(error.to_string()),
                )
                .await;
            }
            return;
        }
        MonitorSetupDecision::Continue => {}
    }

    let _resources = dedicated.resources;
    let mut stream = pubsub.on_message();
    let mut processed = 0_u64;
    let mut succeeded = 0_u64;

    loop {
        if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
            return;
        }

        tokio::select! {
            message = stream.next() => {
                match message {
                    Some(message) => {
                        if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
                            return;
                        }
                        processed += 1;
                        succeeded += 1;
                        if let Ok(snapshot) = mark_task_progress(
                            &state,
                            &task_id,
                            processed,
                            succeeded,
                            0,
                            None,
                        )
                        .await
                        {
                            let channel = message.get_channel_name().to_string();
                            let pattern = message.get_pattern::<Option<String>>().unwrap_or(None);
                            let payload = message
                                .get_payload::<String>()
                                .unwrap_or_else(|_| {
                                    String::from_utf8_lossy(message.get_payload_bytes()).into_owned()
                                });
                            if let Some(event) = pubsub_progress_event_for_snapshot(
                                &snapshot,
                                &channel,
                                pattern.as_deref(),
                                &payload,
                            )
                            {
                                emit_task_event(&app, event);
                            }
                        }
                    }
                    None => {
                        if subscription_stream_end_status(&cancel) == TaskStatus::Cancelled {
                            let _ =
                                emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await;
                            return;
                        }
                        if let Ok(snapshot) = mark_task_completed(&state, &task_id).await {
                            emit_task_event(
                                &app,
                                progress_event(
                                    &snapshot,
                                    vec![],
                                    Some("Subscription connection closed".into()),
                                ),
                            );
                        }
                        return;
                    }
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
    }
}

struct BatchDeleteRun {
    app: AppHandle,
    state: AppState,
    task_id: String,
    connection_id: String,
    explicit_keys: Vec<String>,
    patterns: Vec<String>,
    dry_run: bool,
    scan_count: usize,
    cancel: TaskCancelToken,
}

struct BatchTtlRun {
    app: AppHandle,
    state: AppState,
    task_id: String,
    connection_id: String,
    explicit_keys: Vec<String>,
    patterns: Vec<String>,
    spec: TtlSpec,
    dry_run: bool,
    scan_count: usize,
    cancel: TaskCancelToken,
}

/// 要设的 TTL。三种模式与 `doc/frontend/ttl.md`「三个模式直接对应三条命令」一一对应。
///
/// 与下面的 `apply_ttl_batch` / `apply_ttl_for_connection` 一样是 `pub` 的 ——
/// 任务的启动入口要 `AppHandle`，e2e 里没有，所以真正接触服务端的这一层
/// 单独暴露出来供 e2e 直接驱动（`scan_one_page` 是同样的先例）。
/// 「真实服务端回什么形状」只有在那里才测得到。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TtlSpec {
    pub mode: TtlMode,
    pub value: Option<i64>,
    pub condition: Option<TtlCondition>,
}

/// 批量 TTL 的分块大小。
///
/// 批量删除用 5000，因为那是**一条** `DEL` 的 5000 个参数。TTL 没有 `MEXPIRE`，
/// 一 key 一条命令，5000 条独立命令的请求体积与服务端解析开销高一个量级，
/// 回包却只是 5000 个 integer。1000 在往返次数与单批体积之间更平。
const BATCH_TTL_CHUNK: usize = 1000;

/// 一条 TTL 命令的结局。
///
/// `EXPIRE` / `PERSIST` 返回 1 表示真的改了，返回 0 表示没改 —— key 不存在、
/// 条件不满足、或者 `PERSIST` 时它本来就没有 TTL。**0 既不是成功也不是失败**，
/// 混进任何一边都会让界面上的数字对不上，见 `doc/backend/tasks.md`「进度口径」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TtlReplyOutcome {
    Changed,
    Unchanged,
    Failed,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchTtlProgress {
    pub processed: u64,
    pub succeeded: u64,
    pub failed: u64,
}

impl BatchTtlProgress {
    fn record(&mut self, outcome: TtlReplyOutcome) {
        self.processed += 1;
        match outcome {
            TtlReplyOutcome::Changed => self.succeeded += 1,
            TtlReplyOutcome::Failed => self.failed += 1,
            // 「未变更」不单独记 —— `TaskStatusSnapshot` 只有三个计数，
            // 不为一个任务加字段去污染所有任务。前端算
            // `processed - succeeded - failed` 得到它。
            TtlReplyOutcome::Unchanged => {}
        }
    }

    fn add(&mut self, other: BatchTtlProgress) {
        self.processed += other.processed;
        self.succeeded += other.succeeded;
        self.failed += other.failed;
    }
}

struct MemoryAnalysisRun {
    app: AppHandle,
    state: AppState,
    task_id: String,
    connection_id: String,
    pattern: Option<String>,
    count: usize,
    min_size: Option<u64>,
    max_results: usize,
    cancel: TaskCancelToken,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
struct BatchDeleteProgress {
    processed: u64,
    succeeded: u64,
    failed: u64,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
struct MemoryAnalysisProgress {
    processed: u64,
    succeeded: u64,
    failed: u64,
}

/// 批量删除的分块大小：**一条** `DEL` 的参数个数。
///
/// 单机（以及集群下跨槽位、只能逐 key 的显式列表）走这条。5000 个参数换一次
/// 往返、一个 integer 回包，非常划算 —— 与 `BATCH_TTL_CHUNK` 那 1000 的差别
/// 不在数量而在**形状**：那边没有 `MEXPIRE`，一 key 一条命令。
const BATCH_DELETE_CHUNK: usize = 5000;

/// 集群下按节点 pipeline 那条路的分块大小。
///
/// 形状变了：不是「一条命令 5000 个参数」，而是 5000 条独立的单 key `DEL`
/// （为什么不能合成一条，见 `delete_key_chunk`）。5000 条独立命令的请求体积与
/// 服务端解析开销比前者高一个量级，回包却只是 5000 个 integer —— 与
/// `BATCH_TTL_CHUNK` 同理，1000 在往返次数与单批体积之间更平。
const BATCH_DELETE_CLUSTER_CHUNK: usize = 1000;

/// 这一批该切多大，取决于它会以什么形状发出去。
///
/// 有落点 = 集群下分节点扫回来的一批 = pipeline of 单 key `DEL`。
/// 没落点 = 单机的一条多 key `DEL`，或集群下跨槽位的逐 key 串行 ——
/// 后者切多大只影响进度上报与取消检查点的粒度。
fn batch_delete_chunk_size(has_node: bool) -> usize {
    if has_node {
        BATCH_DELETE_CLUSTER_CHUNK
    } else {
        BATCH_DELETE_CHUNK
    }
}

fn record_batch_delete_success(progress: &mut BatchDeleteProgress, chunk_len: usize, deleted: u64) {
    progress.processed += chunk_len as u64;
    progress.succeeded += deleted;
}

fn should_honor_cancel_after_delete(
    has_more_current_chunks: bool,
    has_more_explicit_or_pattern_work: bool,
    has_more_scan_pages: bool,
    has_more_patterns: bool,
) -> bool {
    has_more_current_chunks
        || has_more_explicit_or_pattern_work
        || has_more_scan_pages
        || has_more_patterns
}

fn should_honor_cancel_after_memory_key(
    has_more_current_keys: bool,
    has_more_scan_pages: bool,
) -> bool {
    has_more_current_keys || has_more_scan_pages
}

async fn run_batch_delete_task(run: BatchDeleteRun) {
    let BatchDeleteRun {
        app,
        state,
        task_id,
        connection_id,
        explicit_keys,
        patterns,
        dry_run,
        scan_count,
        cancel,
    } = run;
    let mut progress = BatchDeleteProgress::default();

    // 勾选来的显式列表。集群下它跨槽位，没有统一落点 —— 传 `None`，
    // `delete_key_chunk` 据此退化成逐 key。
    let explicit_chunks = chunk_keys(explicit_keys, batch_delete_chunk_size(false));
    let has_pattern_work = !patterns.is_empty();
    for (chunk_index, chunk) in explicit_chunks.iter().enumerate() {
        if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
            return;
        }

        match delete_chunk_for_connection(&state, &connection_id, chunk, None, dry_run).await {
            Ok(count) => {
                record_batch_delete_success(&mut progress, chunk.len(), count);
                emit_progress_counts(
                    &app,
                    &state,
                    &task_id,
                    progress.processed,
                    progress.succeeded,
                    progress.failed,
                )
                .await;
            }
            Err(error) => {
                if is_connection_not_found(&error) {
                    emit_connection_closed(&app, &state, &task_id).await;
                    return;
                }
                emit_failed_for_error(&app, &state, &task_id, &error).await;
                return;
            }
        }

        let has_more_explicit_chunks = chunk_index + 1 < explicit_chunks.len();
        if should_honor_cancel_after_delete(
            has_more_explicit_chunks,
            has_pattern_work,
            false,
            false,
        ) && emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await
        {
            return;
        }
    }

    let pattern_count = patterns.len();
    for (pattern_index, pattern) in patterns.into_iter().enumerate() {
        let mut cursor = "0".to_string();
        loop {
            if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
                return;
            }

            // 分节点扫描，**把落点留住**。拍平就丢了，集群下每个 chunk 只能退化成
            // 逐 key 一次往返 —— 树右键一个百万级前缀时那是不可用的。
            // 见 `doc/backend/cluster.md`「批量写：按节点整批或逐 key」。
            let page = scan_page_with_nodes_for_connection(
                &state,
                &connection_id,
                &cursor,
                Some(&pattern),
                scan_count,
            )
            .await;

            if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
                return;
            }

            let page = match page {
                Ok(page) => page,
                Err(error) => {
                    if is_connection_not_found(&error) {
                        emit_connection_closed(&app, &state, &task_id).await;
                    } else if let Ok(event) =
                        mark_task_failed_event(&state, &task_id, error.payload().message).await
                    {
                        emit_task_event(&app, event);
                    }
                    return;
                }
            };

            let has_more_scan_pages = page.cursor != "0";
            let has_more_patterns = pattern_index + 1 < pattern_count;
            let batch_count = page.batches.len();
            for (batch_index, batch) in page.batches.iter().enumerate() {
                // 集群下这一批全部来自同一个节点，整批打给它是安全的。
                let node = batch
                    .node
                    .as_ref()
                    .map(|(host, port)| (host.as_str(), *port));
                let chunks = batch.keys.chunks(batch_delete_chunk_size(node.is_some()));
                let chunk_count = chunks.len();
                for (chunk_index, chunk) in chunks.enumerate() {
                    if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
                        return;
                    }

                    match delete_chunk_for_connection(&state, &connection_id, chunk, node, dry_run)
                        .await
                    {
                        Ok(count) => {
                            record_batch_delete_success(&mut progress, chunk.len(), count);
                            emit_progress_counts(
                                &app,
                                &state,
                                &task_id,
                                progress.processed,
                                progress.succeeded,
                                progress.failed,
                            )
                            .await;
                        }
                        Err(error) => {
                            if is_connection_not_found(&error) {
                                emit_connection_closed(&app, &state, &task_id).await;
                                return;
                            }
                            emit_failed_for_error(&app, &state, &task_id, &error).await;
                            return;
                        }
                    }

                    // 「这一页还有活」现在包含「本批还有 chunk」和「后面还有别的批」。
                    // 单机永远只有一批，所以这条与分批之前完全等价。
                    let has_more_page_chunks =
                        chunk_index + 1 < chunk_count || batch_index + 1 < batch_count;
                    if should_honor_cancel_after_delete(
                        has_more_page_chunks,
                        false,
                        has_more_scan_pages,
                        has_more_patterns,
                    ) && emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await
                    {
                        return;
                    }
                }
            }

            cursor = page.cursor;
            if cursor == "0" {
                break;
            }
        }
    }

    let message = batch_delete_completed_message(progress, dry_run);
    if let Ok(snapshot) = mark_batch_delete_completed(&state, &task_id, message).await {
        let message = snapshot.message.clone();
        emit_task_event(&app, progress_event(&snapshot, vec![], message));
    }
}

async fn run_batch_ttl_task(run: BatchTtlRun) {
    let BatchTtlRun {
        app,
        state,
        task_id,
        connection_id,
        explicit_keys,
        patterns,
        spec,
        dry_run,
        scan_count,
        cancel,
    } = run;
    let mut progress = BatchTtlProgress::default();

    // 勾选来的显式列表。集群下它跨槽位，没有统一落点 —— 传 `None`，
    // `apply_ttl_batch` 据此退化成逐 key。
    for chunk in chunk_keys(explicit_keys, BATCH_TTL_CHUNK) {
        if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
            return;
        }
        match apply_ttl_for_connection(&state, &connection_id, &chunk, None, &spec, dry_run).await {
            Ok(outcome) => {
                progress.add(outcome);
                emit_progress_counts(
                    &app,
                    &state,
                    &task_id,
                    progress.processed,
                    progress.succeeded,
                    progress.failed,
                )
                .await;
            }
            Err(error) => {
                emit_batch_ttl_failure(&app, &state, &task_id, &error).await;
                return;
            }
        }
    }

    for pattern in patterns {
        let mut cursor = "0".to_string();
        loop {
            if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
                return;
            }

            let page = match scan_page_with_nodes_for_connection(
                &state,
                &connection_id,
                &cursor,
                Some(&pattern),
                scan_count,
            )
            .await
            {
                Ok(page) => page,
                Err(error) => {
                    emit_batch_ttl_failure(&app, &state, &task_id, &error).await;
                    return;
                }
            };

            for batch in &page.batches {
                // 集群下这一批全部来自同一个节点，整批 pipeline 打给它是安全的。
                let node = batch
                    .node
                    .as_ref()
                    .map(|(host, port)| (host.as_str(), *port));
                for chunk in batch.keys.chunks(BATCH_TTL_CHUNK) {
                    if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
                        return;
                    }
                    match apply_ttl_for_connection(
                        &state,
                        &connection_id,
                        chunk,
                        node,
                        &spec,
                        dry_run,
                    )
                    .await
                    {
                        Ok(outcome) => {
                            progress.add(outcome);
                            emit_progress_counts(
                                &app,
                                &state,
                                &task_id,
                                progress.processed,
                                progress.succeeded,
                                progress.failed,
                            )
                            .await;
                        }
                        Err(error) => {
                            emit_batch_ttl_failure(&app, &state, &task_id, &error).await;
                            return;
                        }
                    }
                }
            }

            cursor = page.cursor;
            if cursor == "0" {
                break;
            }
        }
    }

    let message = batch_ttl_completed_message(progress, dry_run);
    if let Ok(snapshot) = mark_batch_ttl_completed(&state, &task_id, message).await {
        let message = snapshot.message.clone();
        emit_task_event(&app, progress_event(&snapshot, vec![], message));
    }
}

/// 中途出错的统一出口：连接没了和命令失败要报不同的事件。
async fn emit_batch_ttl_failure(
    app: &AppHandle,
    state: &AppState,
    task_id: &str,
    error: &AppError,
) {
    if is_connection_not_found(error) {
        emit_connection_closed(app, state, task_id).await;
    } else {
        emit_failed_for_error(app, state, task_id, error).await;
    }
}

pub async fn apply_ttl_for_connection(
    state: &AppState,
    connection_id: &str,
    keys: &[String],
    node: Option<(&str, u16)>,
    spec: &TtlSpec,
    dry_run: bool,
) -> AppResult<BatchTtlProgress> {
    if dry_run {
        // 试算只量、不碰服务端；扫描那一半照跑 —— 那正是要量的东西。
        return Ok(BatchTtlProgress {
            processed: keys.len() as u64,
            succeeded: 0,
            failed: 0,
        });
    }
    let runtime = require_session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;
    let result = apply_ttl_batch(&mut executor, keys, node, spec).await;
    capabilities::guard(
        state,
        connection_id,
        &runtime,
        CapabilityId::ExpireConditions,
        result,
    )
}

/// 带条件 flag 时服务端「不认识这个参数」：建连时的能力判断错了，整批都会同样失败。
/// 当成任务失败交给 `capabilities::guard` 降级，而不是记成一串 failed 让用户去猜。
fn unsupported_condition(spec: &TtlSpec, detail: &str) -> bool {
    spec.condition.is_some()
        && capabilities::looks_unsupported(CapabilityId::ExpireConditions, detail)
}

pub async fn apply_ttl_batch(
    executor: &mut RedisExecutor,
    keys: &[String],
    node: Option<(&str, u16)>,
    spec: &TtlSpec,
) -> AppResult<BatchTtlProgress> {
    if keys.is_empty() {
        return Ok(BatchTtlProgress::default());
    }

    // 集群下没有落点 = 这批 key 跨槽位（勾选来的显式列表），只能逐 key 串行。
    // 按槽位分组得引入 slot→node 映射表，v0.3 不付这个复杂度 ——
    // 勾选的量级是几十到几千，不是百万。
    if executor.is_cluster() && node.is_none() {
        let mut progress = BatchTtlProgress::default();
        for key in keys {
            let command = values::build_ttl_command(spec.mode, key, spec.value, spec.condition)?;
            // 单条被拒（ACL、rename-command）记一次 failed 就好，不炸掉整批 ——
            // 与 pipeline 那条路径的口径一致。连接层的错误会在下一个 chunk
            // 取 runtime 时被 `require_session_runtime` 抓到。
            let outcome = match executor.query_raw(command).await {
                Ok(value) => ttl_reply_outcome(Some(&value)),
                Err(AppError::RedisCommandFailed(detail))
                    if unsupported_condition(spec, &detail) =>
                {
                    return Err(AppError::RedisCommandFailed(detail));
                }
                Err(_) => TtlReplyOutcome::Failed,
            };
            progress.record(outcome);
        }
        return Ok(progress);
    }

    let mut pipeline = redis::pipe();
    for key in keys {
        pipeline.add_command(values::build_ttl_command(
            spec.mode,
            key,
            spec.value,
            spec.condition,
        )?);
    }
    let target = match node {
        Some((host, port)) => PipelineTarget::Node {
            host: host.to_string(),
            port,
        },
        // 单机下落点被忽略，借第一个 key 只是为了满足类型
        None => PipelineTarget::Key(keys[0].as_str()),
    };
    let replies = executor.query_pipeline_raw(pipeline, target).await?;

    if let Some(detail) = replies.iter().find_map(|reply| match reply {
        redis::Value::ServerError(error) => Some(error.to_string()),
        _ => None,
    }) {
        if unsupported_condition(spec, &detail) {
            return Err(AppError::RedisCommandFailed(detail));
        }
    }

    let mut progress = BatchTtlProgress::default();
    for index in 0..keys.len() {
        progress.record(ttl_reply_outcome(replies.get(index)));
    }
    Ok(progress)
}

async fn run_import_keys_task(
    app: AppHandle,
    state: AppState,
    task_id: String,
    connection_id: String,
    file_path: String,
    cancel: TaskCancelToken,
) {
    let content = match tokio::fs::read_to_string(&file_path).await {
        Ok(content) => content,
        Err(error) => {
            emit_failed_for_error(
                &app,
                &state,
                &task_id,
                &AppError::NativeFailed(error.to_string()),
            )
            .await;
            return;
        }
    };
    let mut processed = 0_u64;
    let mut succeeded = 0_u64;
    let mut failed = 0_u64;
    let mut first_failure_summary: Option<String> = None;
    for line in content.lines() {
        if cancel.load(Ordering::SeqCst) {
            if let Ok(snapshot) = cancel_task(&state, &task_id).await {
                emit_task_event(&app, progress_event(&snapshot, vec![], None));
            }
            return;
        }
        processed += 1;
        match parse_key_dump_csv_line(line) {
            Ok(row) => {
                let mut command = redis::cmd("RESTORE");
                command
                    .arg(row.key)
                    .arg(row.ttl)
                    .arg(row.payload)
                    .arg("REPLACE");
                match query_raw_for_connection(&state, &connection_id, command).await {
                    Ok(_) => succeeded += 1,
                    Err(_) => {
                        failed += 1;
                        first_failure_summary.get_or_insert_with(|| "RESTORE failed".to_string());
                    }
                }
            }
            Err(error) => {
                failed += 1;
                first_failure_summary.get_or_insert_with(|| sanitized_task_error_detail(&error));
            }
        }
        emit_progress_counts(&app, &state, &task_id, processed, succeeded, failed).await;
    }
    if let Ok(snapshot) = mark_task_completed(&state, &task_id).await {
        let message = import_completed_message(failed, first_failure_summary.as_deref());
        emit_task_event(
            &app,
            progress_event(
                &snapshot,
                vec![TaskEventItem::File { path: file_path }],
                message,
            ),
        );
    }
}

async fn run_import_commands_task(
    app: AppHandle,
    state: AppState,
    task_id: String,
    connection_id: String,
    file_path: String,
    cancel: TaskCancelToken,
) {
    let file = match tokio::fs::File::open(&file_path).await {
        Ok(file) => file,
        Err(error) => {
            emit_failed_for_error(
                &app,
                &state,
                &task_id,
                &AppError::NativeFailed(error.to_string()),
            )
            .await;
            return;
        }
    };
    let mut lines = BufReader::new(file).lines();
    let mut processed = 0_u64;
    let mut succeeded = 0_u64;
    let mut failed = 0_u64;
    let mut line_number = 0_u64;
    let mut first_failure_summary: Option<String> = None;
    loop {
        if cancel.load(Ordering::SeqCst) {
            if let Ok(snapshot) = cancel_task(&state, &task_id).await {
                emit_task_event(&app, progress_event(&snapshot, vec![], None));
            }
            return;
        }
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                emit_failed_for_error(
                    &app,
                    &state,
                    &task_id,
                    &AppError::NativeFailed(error.to_string()),
                )
                .await;
                return;
            }
        };
        line_number += 1;
        match command_import_line_action(&line) {
            CommandImportLineAction::Skip => continue,
            CommandImportLineAction::Execute(command_line) => {
                processed += 1;
                match crate::services::connection::execute_cli(
                    &state,
                    &connection_id,
                    &command_line,
                )
                .await
                {
                    Ok(_) => succeeded += 1,
                    Err(error) => {
                        failed += 1;
                        first_failure_summary.get_or_insert_with(|| {
                            command_import_failure_summary(line_number, &error)
                        });
                    }
                }
                emit_progress_counts(&app, &state, &task_id, processed, succeeded, failed).await;
            }
        }
    }
    if let Ok(snapshot) = mark_task_completed(&state, &task_id).await {
        emit_task_event(
            &app,
            progress_event(
                &snapshot,
                vec![TaskEventItem::File { path: file_path }],
                command_import_completed_message(failed, first_failure_summary.as_deref()),
            ),
        );
    }
}

async fn run_export_keys_task(
    app: AppHandle,
    state: AppState,
    task_id: String,
    connection_id: String,
    keys: Vec<Vec<u8>>,
    output_path: String,
    cancel: TaskCancelToken,
) {
    let mut lines = Vec::new();
    let mut processed = 0_u64;
    let mut succeeded = 0_u64;
    let mut failed = 0_u64;
    for key in keys {
        if cancel.load(Ordering::SeqCst) {
            if let Ok(snapshot) = cancel_task(&state, &task_id).await {
                emit_task_event(&app, progress_event(&snapshot, vec![], None));
            }
            return;
        }
        processed += 1;
        let dump = query_bulk_bytes(&state, &connection_id, "DUMP", &key).await;
        let ttl = query_integer(&state, &connection_id, "PTTL", &key).await;
        match (dump, ttl) {
            (Ok(Some(payload)), Ok(ttl)) => {
                match prepare_export_key_dump_csv_line(&key, &payload, ttl) {
                    Ok(line) => {
                        lines.push(line);
                        succeeded += 1;
                    }
                    Err(_) => failed += 1,
                }
            }
            _ => failed += 1,
        }
        emit_progress_counts(&app, &state, &task_id, processed, succeeded, failed).await;
    }
    if should_cancel_export_before_write(&cancel) == ExportWriteDecision::Cancelled {
        if let Ok(snapshot) = cancel_task(&state, &task_id).await {
            emit_task_event(&app, progress_event(&snapshot, vec![], None));
        }
        return;
    }
    if let Err(error) = tokio::fs::write(&output_path, lines.join("\n")).await {
        emit_failed_for_error(
            &app,
            &state,
            &task_id,
            &AppError::NativeFailed(error.to_string()),
        )
        .await;
        return;
    }
    if let Ok(snapshot) = mark_task_completed(&state, &task_id).await {
        emit_task_event(
            &app,
            progress_event(
                &snapshot,
                vec![TaskEventItem::File { path: output_path }],
                None,
            ),
        );
    }
}

async fn run_memory_analysis_task(run: MemoryAnalysisRun) {
    let MemoryAnalysisRun {
        app,
        state,
        task_id,
        connection_id,
        pattern,
        count,
        min_size,
        max_results,
        cancel,
    } = run;
    let mut cursor = "0".to_string();
    let mut processed = 0_u64;
    let mut succeeded = 0_u64;
    let mut failed = 0_u64;
    let mut emitted = 0_usize;

    if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
        return;
    }

    if max_results == 0 {
        if let Ok(snapshot) = mark_memory_analysis_completed(&state, &task_id).await {
            emit_task_event(&app, progress_event(&snapshot, vec![], None));
        }
        return;
    }

    loop {
        if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
            return;
        }

        let page =
            scan_page_for_connection(&state, &connection_id, &cursor, pattern.as_deref(), count)
                .await;

        if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
            return;
        }

        let (next_cursor, keys) = match page {
            Ok(page) => page,
            Err(error) => {
                if is_connection_not_found(&error) {
                    emit_connection_closed(&app, &state, &task_id).await;
                } else {
                    emit_failed_for_error(&app, &state, &task_id, &error).await;
                }
                return;
            }
        };

        let mut items = Vec::new();
        let key_count = keys.len();
        let has_more_scan_pages = next_cursor != "0";
        for (key_index, key) in keys.into_iter().enumerate() {
            if emitted >= max_results {
                break;
            }

            if emit_cancelled_if_requested(&app, &state, &task_id, &cancel).await {
                return;
            }

            let size = memory_usage_for_connection(&state, &connection_id, &key).await;

            processed += 1;
            match size {
                Ok(size) => {
                    succeeded += 1;
                    if let Some(item) =
                        memory_result_item_for_success(key, size, min_size, emitted, max_results)
                    {
                        emitted += 1;
                        items.push(item);
                    }
                }
                Err(error) => {
                    if is_connection_not_found(&error) {
                        emit_connection_closed(&app, &state, &task_id).await;
                        return;
                    }
                    failed += 1;
                    if let Some(item) =
                        memory_result_item_for_error(key, min_size, emitted, max_results)
                    {
                        emitted += 1;
                        items.push(item);
                    }
                }
            }

            if cancel.load(Ordering::SeqCst) {
                let has_more_current_keys = key_index + 1 < key_count && emitted < max_results;
                let has_more_scan_pages = has_more_scan_pages && emitted < max_results;
                if should_honor_cancel_after_memory_key(has_more_current_keys, has_more_scan_pages)
                {
                    let _ = cancel_task(&state, &task_id).await;
                    emit_memory_progress(
                        &app,
                        &state,
                        &task_id,
                        MemoryAnalysisProgress {
                            processed,
                            succeeded,
                            failed,
                        },
                        items,
                    )
                    .await;
                    return;
                }
            }
        }

        emit_memory_progress(
            &app,
            &state,
            &task_id,
            MemoryAnalysisProgress {
                processed,
                succeeded,
                failed,
            },
            items,
        )
        .await;

        cursor = next_cursor;
        if cursor == "0" || emitted >= max_results {
            if let Ok(snapshot) = mark_memory_analysis_completed(&state, &task_id).await {
                emit_task_event(&app, progress_event(&snapshot, vec![], None));
            }
            return;
        }
    }
}

pub async fn register_task(
    state: &AppState,
    connection_id: String,
    connection_name: String,
    task_type: String,
) -> TaskStatusSnapshot {
    let task_id = uuid::Uuid::new_v4().to_string();
    let record = TaskRecord {
        committing: false,
        task_id: task_id.clone(),
        connection_id,
        connection_name,
        task_type,
        status: TaskStatus::Started,
        processed: 0,
        succeeded: 0,
        failed: 0,
        total: None,
        estimated_progress: None,
        message: None,
        cancel: Arc::new(AtomicBool::new(false)),
        paused: Arc::new(AtomicBool::new(false)),
    };
    let snapshot = snapshot(&record);
    state.tasks.lock().await.insert(task_id, record);
    snapshot
}

pub async fn cancel_task(state: &AppState, task_id: &str) -> AppResult<TaskStatusSnapshot> {
    let mut tasks = state.tasks.lock().await;
    let record = tasks
        .get_mut(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    if record.task_type == "clone_key"
        && matches!(record.status, TaskStatus::Started | TaskStatus::Progress)
    {
        if record.committing {
            return Err(AppError::TaskInvalidRequest(
                "目标写入已开始，需等待服务器确认结果".into(),
            ));
        }
        record.cancel.store(true, Ordering::SeqCst);
        record.message = Some("正在取消克隆，目标尚未写入".into());
        return Ok(snapshot(record));
    }
    if transition_task_status(record, TaskStatus::Cancelled) {
        record.cancel.store(true, Ordering::SeqCst);
    }
    Ok(snapshot(record))
}

pub async fn get_task_status(state: &AppState, task_id: &str) -> AppResult<TaskStatusSnapshot> {
    let tasks = state.tasks.lock().await;
    let record = tasks
        .get(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    Ok(snapshot(record))
}

pub async fn mark_task_progress(
    state: &AppState,
    task_id: &str,
    processed: u64,
    succeeded: u64,
    failed: u64,
    total: Option<u64>,
) -> AppResult<TaskStatusSnapshot> {
    let mut tasks = state.tasks.lock().await;
    let record = tasks
        .get_mut(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    if transition_task_status(record, TaskStatus::Progress) {
        record.processed = processed;
        record.succeeded = succeeded;
        record.failed = failed;
        record.total = total;
    }
    Ok(snapshot(record))
}

#[derive(Clone, Copy)]
struct KeyProgressUpdate {
    processed: u64,
    succeeded: u64,
    failed: u64,
    total: Option<u64>,
    /// 扫描任务的近似进度（0..=1）。`ScanProgressEstimator` 自身持有高水位，
    /// 一旦观测成功过就不会再给 `None`，所以这里可以直接落进 record。
    estimated_progress: Option<f64>,
}

async fn emit_task_key_progress_event(
    app: &AppHandle,
    state: &AppState,
    task_id: &str,
    update: KeyProgressUpdate,
    keys: Vec<KeyEntry>,
) -> AppResult<()> {
    let mut tasks = state.tasks.lock().await;
    let record = tasks
        .get_mut(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    if let Some(event) = progress_event_for_key_record(record, update, keys) {
        emit_task_event(app, event);
    }
    Ok(())
}

pub async fn mark_task_completed(state: &AppState, task_id: &str) -> AppResult<TaskStatusSnapshot> {
    let mut tasks = state.tasks.lock().await;
    let record = tasks
        .get_mut(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    transition_task_status(record, TaskStatus::Completed);
    Ok(snapshot(record))
}

/// 批量删除完成。摘要只有试算才有 —— 见 `batch_delete_completed_message`。
async fn mark_batch_delete_completed(
    state: &AppState,
    task_id: &str,
    message: Option<String>,
) -> AppResult<TaskStatusSnapshot> {
    let mut tasks = state.tasks.lock().await;
    let record = tasks
        .get_mut(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    if record.status != TaskStatus::Failed {
        record.status = TaskStatus::Completed;
        record.message = message;
    }
    Ok(snapshot(record))
}

async fn mark_memory_analysis_completed(
    state: &AppState,
    task_id: &str,
) -> AppResult<TaskStatusSnapshot> {
    let mut tasks = state.tasks.lock().await;
    let record = tasks
        .get_mut(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    if record.status != TaskStatus::Failed {
        record.status = TaskStatus::Completed;
    }
    Ok(snapshot(record))
}

/// 批量 TTL 完成。和上面两个的差别是它要带一句摘要 ——
/// 「未变更」这个第三态只能靠 message 说清楚。
async fn mark_batch_ttl_completed(
    state: &AppState,
    task_id: &str,
    message: Option<String>,
) -> AppResult<TaskStatusSnapshot> {
    let mut tasks = state.tasks.lock().await;
    let record = tasks
        .get_mut(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    if record.status != TaskStatus::Failed {
        record.status = TaskStatus::Completed;
        record.message = message;
    }
    Ok(snapshot(record))
}

pub async fn mark_task_failed(
    state: &AppState,
    task_id: &str,
    message: String,
) -> AppResult<TaskStatusSnapshot> {
    let mut tasks = state.tasks.lock().await;
    let record = tasks
        .get_mut(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    if transition_task_status(record, TaskStatus::Failed) {
        record.message = Some(message);
    }
    Ok(snapshot(record))
}

async fn mark_task_failed_event(
    state: &AppState,
    task_id: &str,
    message: String,
) -> AppResult<TaskProgressEvent> {
    let snapshot = mark_task_failed(state, task_id, message).await?;
    Ok(progress_event(&snapshot, vec![], snapshot.message.clone()))
}

pub async fn task_cancel_token(state: &AppState, task_id: &str) -> AppResult<TaskCancelToken> {
    let tasks = state.tasks.lock().await;
    let record = tasks
        .get(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    Ok(record.cancel.clone())
}

pub async fn task_pause_flag(state: &AppState, task_id: &str) -> AppResult<TaskPauseFlag> {
    let tasks = state.tasks.lock().await;
    let record = tasks
        .get(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    Ok(record.paused.clone())
}

/// 设置任务的暂停标志（`redis_set_task_paused` 的实现）。
///
/// **暂停刻意不是一个 `TaskStatus`**：它服务「窗口隐藏时停手、显示时继续」
/// 这一场景 —— 用户根本不在看界面，不需要一个 UI 可见的状态；往枚举里加
/// 一格会级联到前端所有任务消费点（状态标签、终态判断、事件过滤）。
/// 运行时标志 + 循环页间等待就够了，恢复后从当前游标继续。
/// 终态任务上设置暂停无意义但也无害 —— 循环早已退出，不另做校验。
pub async fn set_task_paused(
    state: &AppState,
    task_id: &str,
    paused: bool,
) -> AppResult<TaskStatusSnapshot> {
    let tasks = state.tasks.lock().await;
    let record = tasks
        .get(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    record.paused.store(paused, Ordering::SeqCst);
    Ok(snapshot(record))
}

pub fn progress_event(
    snapshot: &TaskStatusSnapshot,
    items: Vec<TaskEventItem>,
    message: Option<String>,
) -> TaskProgressEvent {
    TaskProgressEvent {
        task_id: snapshot.task_id.clone(),
        connection_id: snapshot.connection_id.clone(),
        connection_name: snapshot.connection_name.clone(),
        task_type: snapshot.task_type.clone(),
        status: snapshot.status.clone(),
        processed: snapshot.processed,
        succeeded: snapshot.succeeded,
        failed: snapshot.failed,
        total: snapshot.total,
        estimated_progress: snapshot.estimated_progress,
        items,
        message,
    }
}

fn progress_event_for_keys(
    snapshot: &TaskStatusSnapshot,
    keys: Vec<KeyEntry>,
) -> Option<TaskProgressEvent> {
    if snapshot.status != TaskStatus::Progress {
        return None;
    }
    let items = keys
        .into_iter()
        .map(|entry| TaskEventItem::Key {
            key: entry.key,
            key_type: entry.key_type,
            ttl: entry.ttl,
        })
        .collect();
    Some(progress_event(snapshot, items, None))
}

fn monitor_progress_event_for_snapshot(
    snapshot: &TaskStatusSnapshot,
    raw: &str,
) -> Option<TaskProgressEvent> {
    if snapshot.status != TaskStatus::Progress {
        return None;
    }
    Some(progress_event(
        snapshot,
        vec![parse_monitor_line(raw)],
        None,
    ))
}

fn pubsub_progress_event_for_snapshot(
    snapshot: &TaskStatusSnapshot,
    channel: &str,
    pattern: Option<&str>,
    message: &str,
) -> Option<TaskProgressEvent> {
    if snapshot.status != TaskStatus::Progress {
        return None;
    }
    Some(progress_event(
        snapshot,
        vec![pubsub_event_item(channel, pattern, message)],
        None,
    ))
}

fn subscription_stream_end_status(cancel: &TaskCancelToken) -> TaskStatus {
    if cancel.load(Ordering::SeqCst) {
        TaskStatus::Cancelled
    } else {
        TaskStatus::Completed
    }
}

fn progress_event_for_key_record(
    record: &mut TaskRecord,
    update: KeyProgressUpdate,
    keys: Vec<KeyEntry>,
) -> Option<TaskProgressEvent> {
    if !transition_task_status(record, TaskStatus::Progress) {
        return None;
    }
    record.processed = update.processed;
    record.succeeded = update.succeeded;
    record.failed = update.failed;
    record.total = update.total;
    record.estimated_progress = update.estimated_progress;
    let snapshot = snapshot(record);
    progress_event_for_keys(&snapshot, keys)
}

pub fn normalize_scan_count(count: Option<usize>) -> usize {
    match count {
        Some(value) if value > 0 => value,
        _ => 2000,
    }
}

pub fn normalize_scan_pattern(pattern: Option<String>) -> Option<String> {
    pattern.filter(|value| !value.trim().is_empty())
}

fn pubsub_event_item(channel: &str, pattern: Option<&str>, message: &str) -> TaskEventItem {
    TaskEventItem::PubSubMessage {
        channel: channel.to_string(),
        pattern: pattern.map(|value| value.to_string()),
        message: message.to_string(),
    }
}

fn normalize_subscription_options(
    options: &crate::models::SubscriptionTaskOptions,
) -> AppResult<(crate::models::SubscriptionKind, Vec<String>)> {
    let values = match options.kind {
        crate::models::SubscriptionKind::Subscribe => &options.channels,
        crate::models::SubscriptionKind::Psubscribe => &options.patterns,
    };
    let values = values
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Err(AppError::TaskInvalidRequest(
            "Subscription requires at least one channel or pattern".into(),
        ));
    }
    Ok((options.kind.clone(), values))
}

pub fn redis_arg_to_string(arg: &crate::models::RedisCommandArg) -> AppResult<String> {
    match arg {
        crate::models::RedisCommandArg::String { value } => Ok(value.clone()),
        crate::models::RedisCommandArg::Binary { .. } => Err(AppError::TaskInvalidRequest(
            "Phase 3A batch delete task accepts UTF-8 keys only".into(),
        )),
    }
}

fn redis_arg_to_bytes(arg: &crate::models::RedisCommandArg) -> AppResult<Vec<u8>> {
    match arg {
        crate::models::RedisCommandArg::String { value } => Ok(value.as_bytes().to_vec()),
        crate::models::RedisCommandArg::Binary { base64 } => {
            base64::engine::general_purpose::STANDARD
                .decode(base64)
                .map_err(|err| AppError::TaskInvalidRequest(err.to_string()))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyDumpCsvRow {
    key: Vec<u8>,
    payload: Vec<u8>,
    ttl: i64,
}

fn parse_key_dump_csv_line(line: &str) -> AppResult<KeyDumpCsvRow> {
    let parts = line.trim_end_matches('\r').split(',').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(AppError::TaskInvalidRequest(
            "Invalid key dump CSV column count".into(),
        ));
    }
    let key = decode_hex(parts[0], "Invalid hex key in CSV line")?;
    let payload = decode_hex(parts[1], "Invalid hex payload in CSV line")?;
    let ttl = parts[2]
        .parse::<i64>()
        .map_err(|_| AppError::TaskInvalidRequest("Invalid TTL in CSV line".into()))?;
    let ttl = normalize_import_ttl_for_restore(ttl)?;
    Ok(KeyDumpCsvRow { key, payload, ttl })
}

fn format_key_dump_csv_line(key: &[u8], payload: &[u8], ttl: i64) -> String {
    format!("{},{},{}", encode_hex(key), encode_hex(payload), ttl)
}

fn prepare_export_key_dump_csv_line(key: &[u8], payload: &[u8], ttl: i64) -> AppResult<String> {
    Ok(format_key_dump_csv_line(
        key,
        payload,
        normalize_export_ttl_for_csv(ttl)?,
    ))
}

fn normalize_export_ttl_for_csv(ttl: i64) -> AppResult<i64> {
    match ttl {
        ttl if ttl >= 0 => Ok(ttl),
        -1 => Ok(0),
        -2 => Err(AppError::TaskInvalidRequest(
            "Key is not exportable because it no longer exists".into(),
        )),
        _ => Err(AppError::TaskInvalidRequest(
            "Invalid key TTL for export".into(),
        )),
    }
}

fn normalize_import_ttl_for_restore(ttl: i64) -> AppResult<i64> {
    match ttl {
        ttl if ttl >= 0 => Ok(ttl),
        -1 => Ok(0),
        _ => Err(AppError::TaskInvalidRequest(
            "Invalid TTL in CSV line".into(),
        )),
    }
}

fn decode_hex(value: &str, message: &str) -> AppResult<Vec<u8>> {
    let bytes = value.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(AppError::TaskInvalidRequest(message.into()));
    }
    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for [high, low] in bytes.as_chunks::<2>().0 {
        let high = decode_hex_nibble(*high, message)?;
        let low = decode_hex_nibble(*low, message)?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

fn decode_hex_nibble(value: u8, message: &str) -> AppResult<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(AppError::TaskInvalidRequest(message.into())),
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn sanitized_task_error_detail(error: &AppError) -> String {
    error
        .payload()
        .detail
        .unwrap_or_else(|| error.payload().message)
}

fn import_completed_message(failed: u64, first_failure_summary: Option<&str>) -> Option<String> {
    if failed == 0 {
        return None;
    }
    let mut message = format!("Imported with {failed} failed rows");
    if let Some(summary) = first_failure_summary.filter(|summary| !summary.is_empty()) {
        message.push_str(": ");
        message.push_str(summary);
    }
    Some(message)
}

/// 分块。批量删除与批量 TTL 共用，但**块大小不同** —— 见 `BATCH_TTL_CHUNK`。
pub fn chunk_keys(keys: Vec<String>, chunk_size: usize) -> Vec<Vec<String>> {
    let chunk_size = chunk_size.max(1);
    keys.chunks(chunk_size)
        .map(|chunk| chunk.to_vec())
        .collect()
}

/// 批量任务的模式**原样使用**，只做 trim。批量删除与批量 TTL 共用。
///
/// 模式来自 `matchPatternFor`（已经是 `cache:page:*` 的形状）或用户手输。
/// v0.3 之前批量删除有自己的一份，会往尾巴上补一个 `*`：对前者会补成 `**`，
/// 对后者更糟 —— 用户输 `cache:page:home` 想精确删掉一个 key，
/// 实际执行的却是 `cache:page:home*`，`home:v2` 与 `homepage` 一起没了。
/// **界面上写的模式必须就是真正执行的模式**，对不可撤销的操作尤其如此。
///
/// 空模式丢掉：空字符串会被 SCAN 当成"不带 MATCH"，那等于对全库动手 ——
/// 不该由一个没填的输入框触发。
fn batch_scan_pattern(pattern: &str) -> Option<String> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        None
    } else {
        Some(pattern.to_string())
    }
}

/// 试算注册成单独的任务类型。
///
/// 不分的话，点「试算」时状态栏会显示「批量删除 正在运行」—— 那一秒用户
/// 会以为自己点错按钮把库删了。前端的 `TASK_TYPE_LABEL` 给这两个后缀
/// 配了「（试算）」的文案。
fn batch_task_type(base: &str, dry_run: bool) -> String {
    if dry_run {
        format!("{base}_dry_run")
    } else {
        base.to_string()
    }
}

/// 一条 TTL 回复的结局。见 `TtlReplyOutcome` 的文档注释。
///
/// `optional_value` 已经把 `ServerError`（pipeline 开了 `ignore_errors`，
/// 被 ACL 或 `rename-command` 拒掉的那一格就是它）、`Nil`、以及认不出的形状
/// 统一成 `None`，所以这里不必再分一次。
fn ttl_reply_outcome(value: Option<&redis::Value>) -> TtlReplyOutcome {
    match redis_values::optional_value::<i64>(value) {
        Some(1) => TtlReplyOutcome::Changed,
        Some(_) => TtlReplyOutcome::Unchanged,
        // 缺格子也走这里 —— 宁可多报一个失败，也不要把没设上的算成设上了
        None => TtlReplyOutcome::Failed,
    }
}

/// 批量 TTL 的参数校验。
///
/// 三条拒绝里有两条是"等于立即删除"的形状。前端已经拦了，**后端必须再拦一次** ——
/// 前端拦不住的路径（以后的脚本入口、被改坏的前端）会变成一次静默的百万级删除。
/// 见 `doc/backend/tasks.md`「批量 TTL 的后端校验」。
///
/// `now_unix` 由调用方传入，测试才不必依赖真实时钟。
fn validate_ttl_spec(spec: &TtlSpec, now_unix: i64) -> AppResult<()> {
    match spec.mode {
        TtlMode::Persist => {
            if spec.condition.is_some() {
                return Err(AppError::TaskInvalidRequest(
                    "PERSIST does not take a condition".into(),
                ));
            }
        }
        TtlMode::Relative => {
            // `EXPIRE key 0` 立刻删掉 key
            if require_ttl_value(spec)? <= 0 {
                return Err(AppError::TaskInvalidRequest(
                    "A relative TTL must be at least one second".into(),
                ));
            }
        }
        TtlMode::Absolute => {
            // `EXPIREAT` 一个过去的时间戳同样是立刻删除
            if require_ttl_value(spec)? <= now_unix {
                return Err(AppError::TaskInvalidRequest(
                    "An absolute TTL must be in the future".into(),
                ));
            }
        }
    }
    Ok(())
}

fn require_ttl_value(spec: &TtlSpec) -> AppResult<i64> {
    spec.value
        .ok_or_else(|| AppError::TaskInvalidRequest("A TTL value is required for this mode".into()))
}

fn current_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// 批量删除完成时的摘要。
///
/// 真跑时不带：计数已经说明一切，`succeeded` 就是 `DEL` 真的删掉的个数。
/// `processed - succeeded` 是「扫到时还在、删的时候已经不在了」（自然过期
/// 或别人先删了），不必单列 —— 界面上不显示 `processed`，
/// 也就不会有批量 TTL 那种"成功 87 是不是漏了 13"的困惑（`doc/backend/tasks.md`「进度口径」）。
///
/// 试算反过来：命中数是它唯一的产物，必须说出来。
fn batch_delete_completed_message(progress: BatchDeleteProgress, dry_run: bool) -> Option<String> {
    dry_run.then(|| format!("Matched {} keys", progress.processed))
}

/// 完成时的摘要。
///
/// **「未变更」必须说出来**：勾了 100 个看到"成功 87"会让人以为漏了 13 个，
/// 然后去翻日志找一个不存在的 bug。见 `doc/backend/tasks.md`「进度口径」。
fn batch_ttl_completed_message(progress: BatchTtlProgress, dry_run: bool) -> Option<String> {
    if dry_run {
        return Some(format!("Matched {} keys", progress.processed));
    }

    let unchanged = progress
        .processed
        .saturating_sub(progress.succeeded)
        .saturating_sub(progress.failed);
    if unchanged == 0 && progress.failed == 0 {
        return None;
    }

    let mut parts = Vec::new();
    if unchanged > 0 {
        parts.push(format!("{unchanged} unchanged"));
    }
    if progress.failed > 0 {
        parts.push(format!("{} failed", progress.failed));
    }
    Some(format!(
        "Set {} keys, {}",
        progress.succeeded,
        parts.join(", ")
    ))
}

pub fn should_include_memory_result(size: u64, min_size: Option<u64>) -> bool {
    min_size.map(|min| size >= min).unwrap_or(true)
}

fn memory_result_item_for_success(
    key: String,
    size: u64,
    min_size: Option<u64>,
    emitted: usize,
    max_results: usize,
) -> Option<TaskEventItem> {
    if emitted >= max_results || !should_include_memory_result(size, min_size) {
        return None;
    }
    Some(TaskEventItem::Memory { key, size })
}

fn memory_result_item_for_error(
    key: String,
    min_size: Option<u64>,
    emitted: usize,
    max_results: usize,
) -> Option<TaskEventItem> {
    if emitted >= max_results || min_size.unwrap_or(0) > 0 {
        return None;
    }
    Some(TaskEventItem::Memory { key, size: 0 })
}

fn parse_monitor_line(raw: &str) -> TaskEventItem {
    fn unknown(raw: &str) -> TaskEventItem {
        TaskEventItem::Monitor {
            timestamp: String::new(),
            database: None,
            source: None,
            args: vec![],
            raw: raw.into(),
        }
    }

    let Some((timestamp, rest)) = raw.split_once(' ') else {
        return unknown(raw);
    };
    let Ok(timestamp_value) = timestamp.parse::<f64>() else {
        return unknown(raw);
    };
    if timestamp.is_empty() || !timestamp_value.is_finite() {
        return unknown(raw);
    }

    let Some(metadata_and_args) = rest.trim_start().strip_prefix('[') else {
        return unknown(raw);
    };
    let Some(metadata_end) = metadata_and_args.find(']') else {
        return unknown(raw);
    };

    let metadata = &metadata_and_args[..metadata_end];
    let args_input = metadata_and_args[metadata_end + 1..].trim_start();
    let mut metadata_parts = metadata.split_whitespace();
    let Some(database) = metadata_parts
        .next()
        .and_then(|database| database.parse::<i64>().ok())
    else {
        return unknown(raw);
    };
    let Some(source) = metadata_parts.next().filter(|source| !source.is_empty()) else {
        return unknown(raw);
    };
    if metadata_parts.next().is_some() {
        return unknown(raw);
    }
    let Some(args) = parse_monitor_quoted_args(args_input) else {
        return unknown(raw);
    };
    if args.is_empty() {
        return unknown(raw);
    }

    TaskEventItem::Monitor {
        timestamp: timestamp.into(),
        database: Some(database),
        source: Some(source.into()),
        args,
        raw: raw.into(),
    }
}

fn parse_monitor_quoted_args(input: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut escaped = false;
    let mut just_closed_quote = false;

    for ch in input.chars() {
        if !in_quote {
            if ch == '"' {
                if just_closed_quote {
                    return None;
                }
                in_quote = true;
                current.clear();
                just_closed_quote = false;
            } else if ch.is_whitespace() {
                just_closed_quote = false;
                continue;
            } else {
                return None;
            }
            continue;
        }

        if escaped {
            let value = match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            };
            current.push(value);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '"' => {
                args.push(current.clone());
                current.clear();
                in_quote = false;
                just_closed_quote = true;
            }
            other => current.push(other),
        }
    }

    if in_quote || escaped {
        return None;
    }

    Some(args)
}

pub fn build_scan_command(cursor: &str, pattern: Option<&str>, count: usize) -> redis::Cmd {
    let mut command = redis::cmd("SCAN");
    command.arg(cursor);
    if let Some(pattern) = pattern.map(str::trim).filter(|value| !value.is_empty()) {
        command.arg("MATCH").arg(pattern);
    }
    command.arg("COUNT").arg(normalize_scan_count(Some(count)));
    command
}

/// 一批 key 及其 pipeline 落点。
///
/// 集群下**不能拿跨槽位的一批去 pipeline** —— redis-rs 只按第一条命令路由整个
/// pipeline，其余的会先被打到错误的节点，被 MOVED 拒掉后再逐条转发——结果正确，
/// 但每条多一跳（见 `doc/backend/cluster.md`「Pipeline 必须说明落点」）。所以扫描出来的 key 必须带着"从哪个节点
/// 来的"一起往下传，下游才能整批发。`node` 为 `None` 的两种情况：单机（落点被
/// 忽略），以及用户勾选来的跨槽位显式列表（只能逐 key）。
pub struct KeyBatch {
    pub keys: Vec<String>,
    pub node: Option<(String, u16)>,
}

/// 一页扫描结果。单机只有一批，集群下每个主节点一批。
pub struct ScanPage {
    pub cursor: String,
    pub batches: Vec<KeyBatch>,
}

impl ScanPage {
    /// 拍平成一串 key。不关心落点的调用方（批量删除、内存分析、导出）用这个。
    fn flatten(self) -> (String, Vec<String>) {
        (
            self.cursor,
            self.batches
                .into_iter()
                .flat_map(|batch| batch.keys)
                .collect(),
        )
    }
}

/// 扫描一页并保留每批 key 的落点。
///
/// 与 `scan_one_page` 是同一份逻辑的两个视图 —— 扫描只有一处实现，
/// 免得集群游标的编解码出现第二份。
pub async fn scan_one_page_with_nodes(
    executor: &mut RedisExecutor,
    cursor: &str,
    pattern: Option<&str>,
    count: usize,
) -> AppResult<ScanPage> {
    if executor.is_cluster() {
        return scan_cluster_one_page(executor, cursor, pattern, count).await;
    }

    let value = executor
        .query_raw(build_scan_command(cursor, pattern, count))
        .await?;
    let (cursor, keys) = connection::parse_scan_response(value)?;
    Ok(ScanPage {
        cursor,
        batches: vec![KeyBatch { keys, node: None }],
    })
}

pub async fn scan_one_page(
    executor: &mut RedisExecutor,
    cursor: &str,
    pattern: Option<&str>,
    count: usize,
) -> AppResult<(String, Vec<String>)> {
    Ok(scan_one_page_with_nodes(executor, cursor, pattern, count)
        .await?
        .flatten())
}

/// 扫一页并给每个 key 补上类型（`with_ttl` 时再补一批 `TTL`）。
///
/// 树的契约是「SCAN 返回的 key 自带类型」（`doc/backend/tasks.md`「事件项带 keyType」），任务路径
/// 与取值路径（`redis_scan_keys`）必须给出同一形状，所以注解复用
/// `connection::annotate_keys`，且发生在同一次持锁交互里。集群下每批 key 来自
/// 同一个主节点，注解 pipeline 必须整批打回那个节点 —— 跨槽位的 pipeline
/// 会被整个路由到错误的节点，这正是 `ScanPage` 保留落点的原因。
///
/// 错误口径与取值路径一致：单条 `TYPE` / `TTL` 被 ACL 挡掉时 pipeline 开着
/// `ignore_errors`，`annotate_keys` 用 `optional_value` 逐格读，那一格降级成
/// `None`、不影响别的 key；整批失败意味着连接层已经坏了 —— 与 SCAN 本身失败
/// 同一处置，往上抛、由调用方把任务判失败。
///
/// 与 `scan_one_page` 一样是 `pub` 的：任务的启动入口要 `AppHandle`，e2e 里
/// 没有，真正接触服务端的这一层单独暴露出来供 e2e 直接驱动。
pub async fn scan_and_annotate_one_page(
    executor: &mut RedisExecutor,
    cursor: &str,
    pattern: Option<&str>,
    count: usize,
    with_ttl: bool,
) -> AppResult<(String, Vec<KeyEntry>)> {
    let page = scan_one_page_with_nodes(executor, cursor, pattern, count).await?;
    let mut entries = Vec::new();
    for batch in page.batches {
        let mut annotated =
            connection::annotate_keys(executor, batch.keys, with_ttl, batch.node).await?;
        entries.append(&mut annotated);
    }
    Ok((page.cursor, entries))
}

/// 删掉一个 chunk，连接从会话里取。批量删除任务的每一批都走这里。
///
/// 与下面的 `delete_key_chunk` 一样是 `pub` 的 —— 任务的启动入口要 `AppHandle`，
/// e2e 里没有，所以真正接触服务端的这一层单独暴露出来供 e2e 直接驱动
/// （`apply_ttl_for_connection` / `scan_one_page` 是同样的先例）。
/// 「真实服务端到底怎么匹配、试算是不是真的没碰它」只有在那里才测得到，
/// 见 `tests/redis_phase5b.rs`。
pub async fn delete_chunk_for_connection(
    state: &AppState,
    connection_id: &str,
    chunk: &[String],
    node: Option<(&str, u16)>,
    dry_run: bool,
) -> AppResult<u64> {
    if dry_run {
        // 试算只量、不碰服务端；扫描那一半照跑 —— 那正是要量的东西。
        // 计数在调用方：`processed` 加 chunk 长度，`succeeded` 加这里返回的 0。
        // 与 `apply_ttl_for_connection` 同一口径，前端两个对话框都读 processed。
        return Ok(0);
    }
    let runtime = require_session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;
    delete_key_chunk(&mut executor, chunk, node).await
}

async fn scan_page_for_connection(
    state: &AppState,
    connection_id: &str,
    cursor: &str,
    pattern: Option<&str>,
    count: usize,
) -> AppResult<(String, Vec<String>)> {
    let runtime = require_session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;
    scan_one_page(&mut executor, cursor, pattern, count).await
}

/// 同上，但保留每批 key 的落点。批量 TTL 用它 —— 丢了落点，
/// 集群下就只能逐 key 串行。
async fn scan_page_with_nodes_for_connection(
    state: &AppState,
    connection_id: &str,
    cursor: &str,
    pattern: Option<&str>,
    count: usize,
) -> AppResult<ScanPage> {
    let runtime = require_session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;
    scan_one_page_with_nodes(&mut executor, cursor, pattern, count).await
}

async fn memory_usage_for_connection(
    state: &AppState,
    connection_id: &str,
    key: &str,
) -> AppResult<u64> {
    let runtime = require_session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;
    memory_usage_for_key(&mut executor, key).await
}

async fn query_raw_for_connection(
    state: &AppState,
    connection_id: &str,
    command: redis::Cmd,
) -> AppResult<redis::Value> {
    let runtime = require_session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;
    executor.query_raw(command).await
}

async fn query_bulk_bytes(
    state: &AppState,
    connection_id: &str,
    command_name: &str,
    key: &[u8],
) -> AppResult<Option<Vec<u8>>> {
    let mut command = redis::cmd(command_name);
    command.arg(key);
    let value = query_raw_for_connection(state, connection_id, command).await?;
    if matches!(value, redis::Value::Nil) {
        return Ok(None);
    }
    let bytes = Vec::<u8>::from_redis_value(value)
        .map_err(|err| AppError::RedisCommandFailed(err.to_string()))?;
    Ok(Some(bytes))
}

async fn query_integer(
    state: &AppState,
    connection_id: &str,
    command_name: &str,
    key: &[u8],
) -> AppResult<i64> {
    let mut command = redis::cmd(command_name);
    command.arg(key);
    let value = query_raw_for_connection(state, connection_id, command).await?;
    i64::from_redis_value(value).map_err(|err| AppError::RedisCommandFailed(err.to_string()))
}

async fn memory_usage_for_key(executor: &mut RedisExecutor, key: &str) -> AppResult<u64> {
    let mut command = redis::cmd("MEMORY");
    command.arg("USAGE").arg(key);
    let value = executor.query_raw(command).await?;
    if matches!(value, redis::Value::Nil) {
        return Ok(0);
    }
    let size: i64 = redis::FromRedisValue::from_redis_value(value)
        .map_err(|e| AppError::RedisCommandFailed(e.to_string()))?;
    Ok(size.max(0) as u64)
}

/// `DEL` 一个 chunk。三种形状，取决于集群与落点。
///
/// - **单机**：一条 `DEL k1 k2 … k5000`，一次往返。这是最优形状。
/// - **集群 + 有落点**（分节点扫回来的一批）：pipeline 一批**单 key** `DEL`
///   打给该节点，一次往返。
/// - **集群 + 没落点**（用户勾选来的显式列表跨槽位）：逐 key 串行。
///   按槽位分组得引入 slot→node 映射表，这个复杂度暂时不付 ——
///   勾选的量级是几十到几千，不是百万。见 `doc/backend/cluster.md`「批量写：按节点整批或逐 key」。
///
/// **集群下即使同一个节点也不能发多 key `DEL`。** 多 key 命令要求**同槽位**，
/// 不只是同节点：一个主节点持有五千多个槽位，一页扫回来的 key 必然散在其中
/// 好几个上，一条 `DEL k1 k2 …` 会被服务端拒成 `CROSSSLOT Keys in request
/// don't hash to the same slot`。所以按节点分组之后仍然是「pipeline of 单 key
/// `DEL`」，**不要**把它"优化"成一条多 key `DEL` —— 单机测试会全绿，集群上炸。
///
/// 和 `delete_chunk_for_connection` 一样是 `pub` 的，理由见那里 —— 拿到
/// executor 就能在 e2e 里直接驱动，不必先有一个 `AppHandle`。
pub async fn delete_key_chunk(
    executor: &mut RedisExecutor,
    keys: &[String],
    node: Option<(&str, u16)>,
) -> AppResult<u64> {
    if keys.is_empty() {
        return Ok(0);
    }

    if !executor.is_cluster() {
        let mut command = redis::cmd("DEL");
        for key in keys {
            command.arg(key);
        }
        return del_reply_count(executor.query_raw(command).await?);
    }

    let Some((host, port)) = node else {
        let mut deleted = 0_u64;
        for key in keys {
            let mut command = redis::cmd("DEL");
            command.arg(key);
            deleted += del_reply_count(executor.query_raw(command).await?)?;
        }
        return Ok(deleted);
    };

    let mut pipeline = redis::pipe();
    for key in keys {
        let mut command = redis::cmd("DEL");
        command.arg(key);
        pipeline.add_command(command);
    }
    let replies = executor
        .query_pipeline_raw(
            pipeline,
            PipelineTarget::Node {
                host: host.to_string(),
                port,
            },
        )
        .await?;

    let mut deleted = 0_u64;
    for index in 0..keys.len() {
        // pipeline 开着 `ignore_errors`：被 ACL 或 `rename-command` 拒掉的是
        // `Value::ServerError` 那一格，而不是整批失败。失败语义与逐 key 那条路
        // 保持一致 —— 只要有一格读不出整数就整批返回 `Err`，让调用方走
        // `emit_failed_for_error` 把任务判失败并中止。
        let count: u64 = redis_values::optional_value::<i64>(replies.get(index))
            .map(|count| count.max(0) as u64)
            .ok_or_else(|| AppError::RedisCommandFailed("DEL failed".to_string()))?;
        deleted += count;
    }
    Ok(deleted)
}

fn del_reply_count(value: redis::Value) -> AppResult<u64> {
    let count: i64 = redis::FromRedisValue::from_redis_value(value)
        .map_err(|e| AppError::RedisCommandFailed(e.to_string()))?;
    Ok(count.max(0) as u64)
}

async fn scan_cluster_one_page(
    executor: &mut RedisExecutor,
    cursor: &str,
    pattern: Option<&str>,
    count: usize,
) -> AppResult<ScanPage> {
    let nodes = connection::query_cluster_nodes(executor).await?;
    let masters = cluster::parse_master_nodes(&nodes)?;
    let cursors = connection::decode_cluster_scan_cursor(cursor, &masters)?;
    let is_initial_scan = cursor == "0";
    let mut next_cursors = HashMap::new();
    let mut batches = Vec::new();

    for master in masters {
        let endpoint = master.endpoint();
        let node_cursor = cursors.get(&endpoint).map(String::as_str).unwrap_or("0");
        if !is_initial_scan && node_cursor == "0" {
            continue;
        }

        let value = executor
            .query_cluster_node_raw(
                build_scan_command(node_cursor, pattern, count),
                master.host.clone(),
                master.port,
            )
            .await?;
        let (next_cursor, node_keys) = connection::parse_scan_response(value)?;
        // 这一批的 key 全部落在该节点持有的槽位上 —— 留住这个信息，
        // 下游（批量 TTL）才能整批 pipeline 打给它而不是逐 key 串行。
        if !node_keys.is_empty() {
            batches.push(KeyBatch {
                keys: node_keys,
                node: Some((master.host, master.port)),
            });
        }
        if next_cursor != "0" {
            next_cursors.insert(endpoint, next_cursor);
        }
    }

    Ok(ScanPage {
        cursor: connection::encode_cluster_scan_cursor(&next_cursors),
        batches,
    })
}

pub fn snapshot(record: &TaskRecord) -> TaskStatusSnapshot {
    TaskStatusSnapshot {
        task_id: record.task_id.clone(),
        connection_id: record.connection_id.clone(),
        connection_name: record.connection_name.clone(),
        task_type: record.task_type.clone(),
        status: record.status.clone(),
        processed: record.processed,
        succeeded: record.succeeded,
        failed: record.failed,
        total: record.total,
        estimated_progress: record.estimated_progress,
        message: record.message.clone(),
    }
}

fn transition_task_status(record: &mut TaskRecord, status: TaskStatus) -> bool {
    if is_terminal_status(&record.status) {
        return false;
    }
    record.status = status;
    true
}

fn is_terminal_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
    )
}

async fn emit_progress_counts(
    app: &AppHandle,
    state: &AppState,
    task_id: &str,
    processed: u64,
    succeeded: u64,
    failed: u64,
) {
    if let Ok(snapshot) = mark_task_counts(state, task_id, processed, succeeded, failed).await {
        if matches!(
            snapshot.status,
            TaskStatus::Progress | TaskStatus::Cancelled
        ) {
            emit_task_event(app, progress_event(&snapshot, vec![], None));
        }
    }
}

async fn emit_memory_progress(
    app: &AppHandle,
    state: &AppState,
    task_id: &str,
    progress: MemoryAnalysisProgress,
    items: Vec<TaskEventItem>,
) {
    if let Ok(snapshot) = mark_task_counts(
        state,
        task_id,
        progress.processed,
        progress.succeeded,
        progress.failed,
    )
    .await
    {
        if matches!(
            snapshot.status,
            TaskStatus::Progress | TaskStatus::Cancelled
        ) {
            emit_task_event(app, progress_event(&snapshot, items, None));
        }
    }
}

/// 记一次计数进度。批量删除 / 内存分析 / 批量 TTL 共用。
///
/// 这三家原本各有一份逐字相同的实现。终态（完成 / 失败）不再被进度覆盖，
/// 取消态保持取消 —— 否则一个还在飞的 chunk 回来会把任务"复活"成进行中。
async fn mark_task_counts(
    state: &AppState,
    task_id: &str,
    processed: u64,
    succeeded: u64,
    failed: u64,
) -> AppResult<TaskStatusSnapshot> {
    let mut tasks = state.tasks.lock().await;
    let record = tasks
        .get_mut(task_id)
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))?;
    if matches!(record.status, TaskStatus::Completed | TaskStatus::Failed) {
        return Ok(snapshot(record));
    }
    if record.status != TaskStatus::Cancelled {
        record.status = TaskStatus::Progress;
    }
    record.processed = processed;
    record.succeeded = succeeded;
    record.failed = failed;
    record.total = None;
    Ok(snapshot(record))
}

async fn emit_cancelled_if_requested(
    app: &AppHandle,
    state: &AppState,
    task_id: &str,
    cancel: &TaskCancelToken,
) -> bool {
    if !cancel.load(Ordering::SeqCst) {
        return false;
    }
    if let Ok(snapshot) = cancel_task(state, task_id).await {
        emit_task_event(app, progress_event(&snapshot, vec![], None));
    }
    true
}

async fn emit_connection_closed(app: &AppHandle, state: &AppState, task_id: &str) {
    if let Ok(event) = mark_task_failed_event(state, task_id, "Connection is closed".into()).await {
        emit_task_event(app, event);
    }
}

async fn emit_failed_for_error(app: &AppHandle, state: &AppState, task_id: &str, error: &AppError) {
    if let Ok(event) = failed_event_for_error(state, task_id, error).await {
        emit_task_event(app, event);
    }
}

async fn failed_event_for_error(
    state: &AppState,
    task_id: &str,
    error: &AppError,
) -> AppResult<TaskProgressEvent> {
    mark_task_failed_event(state, task_id, error.payload().message).await
}

fn is_connection_not_found(error: &AppError) -> bool {
    matches!(error, AppError::ConnectionNotFound(_))
}

#[cfg(test)]
mod tests {
    use super::{
        batch_delete_chunk_size, batch_delete_completed_message, batch_scan_pattern,
        batch_task_type, batch_ttl_completed_message, build_scan_command, cancel_task, chunk_keys,
        command_import_completed_message, command_import_failure_summary,
        command_import_line_action, failed_event_for_error, format_key_dump_csv_line,
        get_task_status, import_completed_message, mark_batch_delete_completed,
        mark_batch_ttl_completed, mark_memory_analysis_completed, mark_task_completed,
        mark_task_counts, mark_task_failed, mark_task_failed_event, mark_task_progress,
        memory_result_item_for_error, memory_result_item_for_success,
        monitor_progress_event_for_snapshot, monitor_setup_decision, normalize_scan_count,
        normalize_scan_pattern, normalize_subscription_options, parse_key_dump_csv_line,
        parse_monitor_line, prepare_export_key_dump_csv_line, progress_event,
        progress_event_for_key_record, progress_event_for_keys, pubsub_event_item,
        pubsub_progress_event_for_snapshot, record_batch_delete_success, redis_arg_to_string,
        register_task, require_connection_name, reversed_cursor_fraction,
        sanitized_task_error_detail, scan_pacing_delay, set_task_paused,
        should_cancel_export_before_write, should_honor_cancel_after_delete,
        should_honor_cancel_after_memory_key, should_include_memory_result,
        should_keep_waiting_while_paused, subscription_stream_end_status, task_cancel_token,
        task_pause_flag, ttl_reply_outcome, validate_ttl_spec, BatchDeleteProgress,
        BatchTtlProgress, CommandImportLineAction, ExportWriteDecision, KeyProgressUpdate,
        MonitorSetupDecision, ScanProgressEstimator, TtlReplyOutcome, TtlSpec,
        SCAN_TASK_PACING_CAP,
    };
    use crate::error::AppError;
    use crate::models::{
        ExportKeysTaskOptions, KeyEntry, KeyScanTaskOptions, RedisCommandArg, SubscriptionKind,
        SubscriptionTaskOptions, TaskEventItem, TaskStatus, TaskStatusSnapshot, TtlCondition,
        TtlMode,
    };
    use crate::state::AppState;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::Duration;

    #[test]
    fn classifies_command_import_lines() {
        assert_eq!(
            command_import_line_action(""),
            CommandImportLineAction::Skip
        );
        assert_eq!(
            command_import_line_action("   "),
            CommandImportLineAction::Skip
        );
        assert_eq!(
            command_import_line_action("SET a 1"),
            CommandImportLineAction::Execute("SET a 1".into())
        );
    }

    #[test]
    fn command_import_completed_message_reports_failed_commands_without_success_message() {
        assert_eq!(
            command_import_completed_message(0, Some("line 7: Redis command failed")),
            None
        );
        assert_eq!(
            command_import_completed_message(3, Some("line 7: Redis command failed")).as_deref(),
            Some("Imported commands with 3 failed commands: line 7: Redis command failed")
        );
    }

    #[test]
    fn command_import_failure_summary_uses_line_number_and_sanitized_error_without_command_text() {
        let error = AppError::RedisCommandFailed("ERR invalid syntax for SET secret-token".into());

        let summary = command_import_failure_summary(42, &error);

        assert_eq!(summary, "line 42: Redis command failed");
        assert!(!summary.contains("SET secret-token"));
        assert!(!summary.contains("ERR invalid syntax"));
    }

    #[test]
    fn parses_and_formats_legacy_key_dump_csv_lines() {
        let row = parse_key_dump_csv_line("757365723a31,000102ff,1500").unwrap();
        assert_eq!(row.key, b"user:1".to_vec());
        assert_eq!(row.payload, vec![0, 1, 2, 255]);
        assert_eq!(row.ttl, 1500);

        assert_eq!(
            format_key_dump_csv_line(b"user:1", &[0, 1, 2, 255], 1500),
            "757365723a31,000102ff,1500"
        );
    }

    #[test]
    fn rejects_invalid_key_dump_csv_lines_without_file_contents_in_error() {
        let err = parse_key_dump_csv_line("not-hex,00,0").unwrap_err();

        assert_eq!(err.payload().code, "redis.task_invalid_request");
        assert_eq!(
            err.payload().detail.as_deref(),
            Some("Invalid hex key in CSV line")
        );
    }

    #[test]
    fn export_csv_maps_persistent_ttl_and_rejects_missing_or_invalid_negative_ttl() {
        assert_eq!(
            prepare_export_key_dump_csv_line(b"user:1", &[0, 1, 2, 255], -1).unwrap(),
            "757365723a31,000102ff,0"
        );

        let missing = prepare_export_key_dump_csv_line(b"user:1", &[0], -2).unwrap_err();
        assert_eq!(missing.payload().code, "redis.task_invalid_request");
        assert_eq!(
            missing.payload().detail.as_deref(),
            Some("Key is not exportable because it no longer exists")
        );

        let invalid = prepare_export_key_dump_csv_line(b"user:1", &[0], -42).unwrap_err();
        assert_eq!(invalid.payload().code, "redis.task_invalid_request");
        assert_eq!(
            invalid.payload().detail.as_deref(),
            Some("Invalid key TTL for export")
        );
    }

    #[test]
    fn import_csv_maps_legacy_persistent_ttl_and_rejects_invalid_negative_ttl() {
        let row = parse_key_dump_csv_line("757365723a31,00,-1").unwrap();
        assert_eq!(row.key, b"user:1".to_vec());
        assert_eq!(row.payload, vec![0]);
        assert_eq!(row.ttl, 0);

        let err = parse_key_dump_csv_line("757365723a31,00,-2").unwrap_err();
        assert_eq!(err.payload().code, "redis.task_invalid_request");
        assert_eq!(
            err.payload().detail.as_deref(),
            Some("Invalid TTL in CSV line")
        );
    }

    #[test]
    fn non_ascii_invalid_hex_returns_sanitized_error_without_panic() {
        let err = parse_key_dump_csv_line("中a,00,0").unwrap_err();

        assert_eq!(err.payload().code, "redis.task_invalid_request");
        assert_eq!(
            err.payload().detail.as_deref(),
            Some("Invalid hex key in CSV line")
        );
    }

    #[test]
    fn export_write_before_cancel_helper_skips_write_when_cancelled() {
        let cancel = Arc::new(AtomicBool::new(true));

        assert_eq!(
            should_cancel_export_before_write(&cancel),
            ExportWriteDecision::Cancelled
        );

        cancel.store(false, Ordering::SeqCst);

        assert_eq!(
            should_cancel_export_before_write(&cancel),
            ExportWriteDecision::Write
        );
    }

    #[test]
    fn import_completed_message_reports_failed_rows_with_sanitized_first_error() {
        let first_error = AppError::TaskInvalidRequest("Invalid TTL in CSV line".into());
        let summary = sanitized_task_error_detail(&first_error);

        assert_eq!(
            import_completed_message(2, Some(&summary)).as_deref(),
            Some("Imported with 2 failed rows: Invalid TTL in CSV line")
        );
        assert_eq!(import_completed_message(0, Some(&summary)), None);
    }

    #[test]
    fn parses_redis_monitor_line_into_event_item() {
        let raw = r#"1710000000.123456 [2 127.0.0.1:60123] "SET" "user:1" "Ada""#;

        let item = parse_monitor_line(raw);

        assert_eq!(
            item,
            TaskEventItem::Monitor {
                timestamp: "1710000000.123456".into(),
                database: Some(2),
                source: Some("127.0.0.1:60123".into()),
                args: vec!["SET".into(), "user:1".into(), "Ada".into()],
                raw: raw.into(),
            }
        );
    }

    #[test]
    fn monitor_parser_preserves_raw_line_when_shape_is_unknown() {
        let raw = "unexpected monitor payload";

        let item = parse_monitor_line(raw);

        assert_eq!(
            item,
            TaskEventItem::Monitor {
                timestamp: "".into(),
                database: None,
                source: None,
                args: vec![],
                raw: raw.into(),
            }
        );
    }

    #[test]
    fn monitor_parser_treats_malformed_header_as_unknown() {
        let raw = r#"1710000000.123456 [] "SET""#;

        assert_eq!(
            parse_monitor_line(raw),
            TaskEventItem::Monitor {
                timestamp: "".into(),
                database: None,
                source: None,
                args: vec![],
                raw: raw.into(),
            }
        );
    }

    #[test]
    fn monitor_parser_treats_non_finite_timestamp_as_unknown() {
        let raw = r#"NaN [0 127.0.0.1:6379] "PING""#;

        assert_eq!(
            parse_monitor_line(raw),
            TaskEventItem::Monitor {
                timestamp: "".into(),
                database: None,
                source: None,
                args: vec![],
                raw: raw.into(),
            }
        );
    }

    #[test]
    fn monitor_progress_event_for_snapshot_only_emits_for_progress() {
        let raw = r#"1710000000.123456 [0 127.0.0.1:6379] "PING""#;
        let mut snapshot = TaskStatusSnapshot {
            task_id: "task-1".into(),
            connection_id: "conn-1".into(),
            connection_name: "local".into(),
            task_type: "monitor".into(),
            status: TaskStatus::Progress,
            processed: 1,
            succeeded: 1,
            failed: 0,
            total: None,
            estimated_progress: None,
            message: None,
        };

        let event = monitor_progress_event_for_snapshot(&snapshot, raw).unwrap();

        assert_eq!(event.status, TaskStatus::Progress);
        assert_eq!(event.items, vec![parse_monitor_line(raw)]);

        snapshot.status = TaskStatus::Cancelled;

        assert_eq!(monitor_progress_event_for_snapshot(&snapshot, raw), None);
    }

    #[test]
    fn monitor_setup_decision_prefers_cancelled_over_failed() {
        let cancel = Arc::new(AtomicBool::new(true));
        let failed: Result<(), AppError> = Err(AppError::RedisConnectionFailed("boom".into()));

        assert_eq!(
            monitor_setup_decision(&failed, &cancel),
            MonitorSetupDecision::Cancelled
        );

        cancel.store(false, Ordering::SeqCst);

        assert_eq!(
            monitor_setup_decision(&failed, &cancel),
            MonitorSetupDecision::Failed
        );
        assert_eq!(
            monitor_setup_decision(&Ok::<_, AppError>(()), &cancel),
            MonitorSetupDecision::Continue
        );
    }

    #[test]
    fn builds_pubsub_task_items_for_message_and_pmessage() {
        assert_eq!(
            pubsub_event_item("news", None, "hello"),
            TaskEventItem::PubSubMessage {
                channel: "news".into(),
                pattern: None,
                message: "hello".into(),
            }
        );
        assert_eq!(
            pubsub_event_item("news:1", Some("news:*"), "hello"),
            TaskEventItem::PubSubMessage {
                channel: "news:1".into(),
                pattern: Some("news:*".into()),
                message: "hello".into(),
            }
        );
    }

    #[test]
    fn normalizes_subscription_options_for_channels_and_patterns() {
        let options = SubscriptionTaskOptions {
            kind: SubscriptionKind::Subscribe,
            channels: vec![" news ".into(), "".into(), "  ".into(), "alerts".into()],
            patterns: vec!["ignored:*".into()],
        };

        let (kind, values) = normalize_subscription_options(&options).unwrap();

        assert_eq!(kind, SubscriptionKind::Subscribe);
        assert_eq!(values, vec!["news", "alerts"]);

        let options = SubscriptionTaskOptions {
            kind: SubscriptionKind::Psubscribe,
            channels: vec!["ignored".into()],
            patterns: vec![" news:* ".into(), "".into()],
        };

        let (kind, values) = normalize_subscription_options(&options).unwrap();

        assert_eq!(kind, SubscriptionKind::Psubscribe);
        assert_eq!(values, vec!["news:*"]);
    }

    #[test]
    fn normalize_subscription_options_rejects_empty_selected_values() {
        let options = SubscriptionTaskOptions {
            kind: SubscriptionKind::Subscribe,
            channels: vec![" ".into()],
            patterns: vec!["news:*".into()],
        };

        let error = normalize_subscription_options(&options).unwrap_err();

        assert!(matches!(error, AppError::TaskInvalidRequest(_)));
        assert_eq!(
            error.payload().detail.as_deref(),
            Some("Subscription requires at least one channel or pattern")
        );
    }

    #[test]
    fn pubsub_progress_event_for_snapshot_only_emits_for_progress() {
        let mut snapshot = TaskStatusSnapshot {
            task_id: "task-1".into(),
            connection_id: "conn-1".into(),
            connection_name: "local".into(),
            task_type: "psubscribe".into(),
            status: TaskStatus::Progress,
            processed: 1,
            succeeded: 1,
            failed: 0,
            total: None,
            estimated_progress: None,
            message: None,
        };

        let event =
            pubsub_progress_event_for_snapshot(&snapshot, "news:1", Some("news:*"), "hello")
                .unwrap();

        assert_eq!(event.status, TaskStatus::Progress);
        assert_eq!(
            event.items,
            vec![TaskEventItem::PubSubMessage {
                channel: "news:1".into(),
                pattern: Some("news:*".into()),
                message: "hello".into(),
            }]
        );

        snapshot.status = TaskStatus::Cancelled;

        assert_eq!(
            pubsub_progress_event_for_snapshot(&snapshot, "news:1", Some("news:*"), "hello"),
            None
        );
    }

    #[test]
    fn subscription_stream_end_status_prefers_cancelled_when_cancel_flag_is_set() {
        let cancel = Arc::new(AtomicBool::new(true));

        assert_eq!(
            subscription_stream_end_status(&cancel),
            TaskStatus::Cancelled
        );

        cancel.store(false, Ordering::SeqCst);

        assert_eq!(
            subscription_stream_end_status(&cancel),
            TaskStatus::Completed
        );
    }

    #[test]
    fn serializes_phase3b_task_items_with_camel_case_payloads() {
        let monitor = TaskEventItem::Monitor {
            timestamp: "1710000000.123456".into(),
            database: Some(0),
            source: Some("127.0.0.1:6379".into()),
            args: vec!["SET".into(), "a".into(), "1".into()],
            raw: r#"1710000000.123456 [0 127.0.0.1:6379] "SET" "a" "1""#.into(),
        };
        let message = TaskEventItem::PubSubMessage {
            channel: "news".into(),
            pattern: None,
            message: "hello".into(),
        };
        let file = TaskEventItem::File {
            path: "D:\\dump.csv".into(),
        };

        assert_eq!(
            serde_json::to_value(monitor).unwrap(),
            serde_json::json!({
                "type": "monitor",
                "timestamp": "1710000000.123456",
                "database": 0,
                "source": "127.0.0.1:6379",
                "args": ["SET", "a", "1"],
                "raw": "1710000000.123456 [0 127.0.0.1:6379] \"SET\" \"a\" \"1\""
            })
        );
        assert_eq!(
            serde_json::to_value(message).unwrap(),
            serde_json::json!({
                "type": "pubSubMessage",
                "channel": "news",
                "pattern": null,
                "message": "hello"
            })
        );
        assert_eq!(
            serde_json::to_value(file).unwrap(),
            serde_json::json!({
                "type": "file",
                "path": "D:\\dump.csv"
            })
        );
    }

    #[test]
    fn serializes_scan_key_items_with_camel_case_payloads() {
        let annotated = TaskEventItem::Key {
            key: "user:1".into(),
            key_type: Some("string".into()),
            ttl: Some(120),
        };
        let bare = TaskEventItem::Key {
            key: "user:2".into(),
            key_type: None,
            ttl: None,
        };

        assert_eq!(
            serde_json::to_value(annotated).unwrap(),
            serde_json::json!({
                "type": "key",
                "key": "user:1",
                "keyType": "string",
                "ttl": 120
            })
        );
        assert_eq!(
            serde_json::to_value(bare).unwrap(),
            serde_json::json!({
                "type": "key",
                "key": "user:2",
                "keyType": null,
                "ttl": null
            })
        );
    }

    #[test]
    fn deserializes_key_scan_task_options_with_camel_case_payloads() {
        let options: KeyScanTaskOptions = serde_json::from_value(serde_json::json!({
            "pattern": "user:*",
            "count": 2000,
            "withTtl": true
        }))
        .unwrap();

        assert_eq!(options.pattern.as_deref(), Some("user:*"));
        assert_eq!(options.count, Some(2000));
        assert_eq!(options.with_ttl, Some(true));

        // withTtl 缺省时也要能解 —— 语义与取值路径一致：缺省不取
        let options: KeyScanTaskOptions =
            serde_json::from_value(serde_json::json!({ "pattern": null, "count": null })).unwrap();
        assert_eq!(options.with_ttl, None);
    }

    #[test]
    fn scan_pacing_matches_page_cost_capped_but_not_after_final_page() {
        // 最后一页无论页耗时多少都不睡
        assert_eq!(scan_pacing_delay("0", Duration::from_millis(3)), None);
        // 本机：睡等量于上一页实测耗时，占空比 ~50%
        assert_eq!(
            scan_pacing_delay("1729", Duration::from_millis(3)),
            Some(Duration::from_millis(3))
        );
        // WAN：页耗时被 RTT 主导时封顶，不比固定间隔时代慢
        assert_eq!(
            scan_pacing_delay("1729", Duration::from_millis(180)),
            Some(SCAN_TASK_PACING_CAP)
        );
        assert_eq!(
            scan_pacing_delay(
                r#"cluster:{"127.0.0.1:7001":"88"}"#,
                Duration::from_millis(5)
            ),
            Some(Duration::from_millis(5))
        );
    }

    #[test]
    fn pause_wait_requires_paused_and_not_cancelled() {
        let paused = Arc::new(AtomicBool::new(true));
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(should_keep_waiting_while_paused(&paused, &cancel));

        // 暂停期间取消要立刻生效，不能等恢复
        cancel.store(true, Ordering::SeqCst);
        assert!(!should_keep_waiting_while_paused(&paused, &cancel));

        cancel.store(false, Ordering::SeqCst);
        paused.store(false, Ordering::SeqCst);
        assert!(!should_keep_waiting_while_paused(&paused, &cancel));
    }

    #[tokio::test]
    async fn set_task_paused_toggles_flag_without_changing_status() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "search_keys".into(),
        )
        .await;
        let flag = task_pause_flag(&state, &started.task_id).await.unwrap();
        assert!(!flag.load(Ordering::SeqCst));

        let snapshot = set_task_paused(&state, &started.task_id, true)
            .await
            .unwrap();
        // 暂停是运行时标志，不是任务状态 —— 快照的 status 不许被它改动
        assert_eq!(snapshot.status, TaskStatus::Started);
        assert!(flag.load(Ordering::SeqCst));

        let snapshot = set_task_paused(&state, &started.task_id, false)
            .await
            .unwrap();
        assert_eq!(snapshot.status, TaskStatus::Started);
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn set_task_paused_on_missing_task_returns_normalized_error() {
        let state = AppState::default();

        let err = set_task_paused(&state, "missing", true).await.unwrap_err();

        assert_eq!(err.payload().code, "redis.task_not_found");
    }

    #[test]
    fn deserializes_subscription_task_options_with_camel_case_payloads() {
        let options: SubscriptionTaskOptions = serde_json::from_value(serde_json::json!({
            "kind": "psubscribe",
            "channels": [],
            "patterns": ["news:*"]
        }))
        .unwrap();

        assert_eq!(options.kind, SubscriptionKind::Psubscribe);
        assert!(options.channels.is_empty());
        assert_eq!(options.patterns, vec!["news:*"]);
    }

    #[test]
    fn deserializes_subscription_kind_from_frontend_strings() {
        assert_eq!(
            serde_json::from_str::<SubscriptionKind>(r#""subscribe""#).unwrap(),
            SubscriptionKind::Subscribe
        );
        assert_eq!(
            serde_json::from_str::<SubscriptionKind>(r#""psubscribe""#).unwrap(),
            SubscriptionKind::Psubscribe
        );
    }

    #[test]
    fn deserializes_export_keys_task_options_with_camel_case_payloads() {
        let options: ExportKeysTaskOptions = serde_json::from_value(serde_json::json!({
            "keys": [{ "type": "string", "value": "user:1" }],
            "outputPath": "D:\\dump.csv"
        }))
        .unwrap();

        assert_eq!(options.output_path, "D:\\dump.csv");
        assert_eq!(
            options.keys,
            vec![RedisCommandArg::String {
                value: "user:1".into()
            }]
        );
    }

    #[tokio::test]
    async fn registers_queries_and_cancels_tasks() {
        let state = AppState::default();

        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "load_all_keys".into(),
        )
        .await;

        assert_eq!(started.connection_id, "conn-1");
        assert_eq!(started.connection_name, "local");
        assert_eq!(started.task_type, "load_all_keys");
        assert_eq!(started.status, TaskStatus::Started);

        let queried = get_task_status(&state, &started.task_id).await.unwrap();
        assert_eq!(queried.task_id, started.task_id);
        assert_eq!(queried.status, TaskStatus::Started);

        let cancel_token = task_cancel_token(&state, &started.task_id).await.unwrap();
        let cancelled = cancel_task(&state, &started.task_id).await.unwrap();
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert!(cancel_token.load(Ordering::SeqCst));

        let queried_after_cancel = get_task_status(&state, &started.task_id).await.unwrap();
        assert_eq!(queried_after_cancel.status, TaskStatus::Cancelled);
    }

    #[tokio::test]
    async fn cancelling_missing_task_returns_normalized_error() {
        let state = AppState::default();

        let err = cancel_task(&state, "missing").await.unwrap_err();

        assert_eq!(err.payload().code, "redis.task_not_found");
    }

    #[tokio::test]
    async fn starting_scan_task_requires_existing_connection() {
        let state = AppState::default();

        let err = require_connection_name(&state, "missing")
            .await
            .unwrap_err();

        assert_eq!(err.payload().code, "redis.connection_not_found");
    }

    #[tokio::test]
    async fn updates_progress_and_builds_event_payload() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "memory_analysis".into(),
        )
        .await;

        let snapshot = mark_task_progress(&state, &started.task_id, 10, 8, 2, None)
            .await
            .unwrap();
        let event = progress_event(
            &snapshot,
            vec![TaskEventItem::Memory {
                key: "user:1".into(),
                size: 42,
            }],
            Some("scanning".into()),
        );

        assert_eq!(snapshot.processed, 10);
        assert_eq!(snapshot.succeeded, 8);
        assert_eq!(snapshot.failed, 2);
        assert_eq!(event.task_id, snapshot.task_id);
        assert_eq!(event.connection_id, "conn-1");
        assert_eq!(event.connection_name, "local");
        assert_eq!(event.task_type, "memory_analysis");
        assert_eq!(event.status, TaskStatus::Progress);
        assert_eq!(event.processed, 10);
        assert_eq!(event.succeeded, 8);
        assert_eq!(event.failed, 2);
        assert_eq!(event.total, None);
        assert_eq!(event.message.as_deref(), Some("scanning"));
        assert_eq!(
            event.items,
            vec![TaskEventItem::Memory {
                key: "user:1".into(),
                size: 42
            }]
        );
    }

    #[tokio::test]
    async fn marks_task_failed_with_message() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "batch_delete".into(),
        )
        .await;

        let snapshot = mark_task_failed(&state, &started.task_id, "delete failed".into())
            .await
            .unwrap();

        assert_eq!(snapshot.status, TaskStatus::Failed);
        assert_eq!(snapshot.message.as_deref(), Some("delete failed"));
    }

    #[tokio::test]
    async fn failed_scan_task_event_uses_failed_snapshot_and_message() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "load_all_keys".into(),
        )
        .await;

        let event = mark_task_failed_event(&state, &started.task_id, "Connection is closed".into())
            .await
            .unwrap();

        assert_eq!(event.task_id, started.task_id);
        assert_eq!(event.connection_id, "conn-1");
        assert_eq!(event.connection_name, "local");
        assert_eq!(event.task_type, "load_all_keys");
        assert_eq!(event.status, TaskStatus::Failed);
        assert_eq!(event.message.as_deref(), Some("Connection is closed"));
        assert!(event.items.is_empty());
    }

    #[test]
    fn terminal_snapshots_do_not_build_key_progress_events() {
        let snapshot = TaskStatusSnapshot {
            task_id: "task-1".into(),
            connection_id: "conn-1".into(),
            connection_name: "local".into(),
            task_type: "load_all_keys".into(),
            status: TaskStatus::Cancelled,
            processed: 2,
            succeeded: 2,
            failed: 0,
            total: None,
            estimated_progress: None,
            message: None,
        };

        let event = progress_event_for_keys(
            &snapshot,
            vec![KeyEntry {
                key: "key:1".into(),
                key_type: Some("string".into()),
                ttl: None,
            }],
        );

        assert_eq!(event, None);
    }

    #[test]
    fn key_progress_events_carry_type_and_ttl_per_entry() {
        let snapshot = TaskStatusSnapshot {
            task_id: "task-1".into(),
            connection_id: "conn-1".into(),
            connection_name: "local".into(),
            task_type: "search_keys".into(),
            status: TaskStatus::Progress,
            processed: 2,
            succeeded: 2,
            failed: 0,
            total: None,
            estimated_progress: None,
            message: None,
        };

        let event = progress_event_for_keys(
            &snapshot,
            vec![
                KeyEntry {
                    key: "user:1".into(),
                    key_type: Some("hash".into()),
                    ttl: Some(120),
                },
                // SCAN 与 TYPE 之间被删掉的 key：类型缺失但仍要上报
                KeyEntry {
                    key: "user:2".into(),
                    key_type: None,
                    ttl: None,
                },
            ],
        )
        .unwrap();

        assert_eq!(
            event.items,
            vec![
                TaskEventItem::Key {
                    key: "user:1".into(),
                    key_type: Some("hash".into()),
                    ttl: Some(120),
                },
                TaskEventItem::Key {
                    key: "user:2".into(),
                    key_type: None,
                    ttl: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn cancelled_task_record_does_not_build_key_progress_event() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "load_all_keys".into(),
        )
        .await;
        cancel_task(&state, &started.task_id).await.unwrap();

        let mut tasks = state.tasks.lock().await;
        let record = tasks.get_mut(&started.task_id).unwrap();
        let event = progress_event_for_key_record(
            record,
            KeyProgressUpdate {
                processed: 1,
                succeeded: 1,
                failed: 0,
                total: None,
                estimated_progress: None,
            },
            vec![KeyEntry {
                key: "key:1".into(),
                key_type: Some("string".into()),
                ttl: None,
            }],
        );

        assert_eq!(event, None);
        assert_eq!(record.status, TaskStatus::Cancelled);
        assert_eq!(record.processed, 0);
    }

    #[tokio::test]
    async fn terminal_states_ignore_later_transitions() {
        let state = AppState::default();
        let cancelled = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "memory_analysis".into(),
        )
        .await;
        cancel_task(&state, &cancelled.task_id).await.unwrap();

        let after_progress = mark_task_progress(&state, &cancelled.task_id, 10, 8, 2, Some(20))
            .await
            .unwrap();
        assert_eq!(after_progress.status, TaskStatus::Cancelled);
        assert_eq!(after_progress.processed, 0);
        assert_eq!(after_progress.total, None);

        let after_completed = mark_task_completed(&state, &cancelled.task_id)
            .await
            .unwrap();
        assert_eq!(after_completed.status, TaskStatus::Cancelled);

        let completed = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "batch_delete".into(),
        )
        .await;
        mark_task_completed(&state, &completed.task_id)
            .await
            .unwrap();
        let after_cancel = cancel_task(&state, &completed.task_id).await.unwrap();
        assert_eq!(after_cancel.status, TaskStatus::Completed);

        let failed = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "batch_delete".into(),
        )
        .await;
        mark_task_failed(&state, &failed.task_id, "delete failed".into())
            .await
            .unwrap();
        let after_failed_progress = mark_task_progress(&state, &failed.task_id, 10, 8, 2, Some(20))
            .await
            .unwrap();
        assert_eq!(after_failed_progress.status, TaskStatus::Failed);
        assert_eq!(
            after_failed_progress.message.as_deref(),
            Some("delete failed")
        );
        assert_eq!(after_failed_progress.processed, 0);
    }

    #[test]
    fn normalizes_scan_options() {
        assert_eq!(normalize_scan_count(None), 2000);
        assert_eq!(normalize_scan_count(Some(0)), 2000);
        assert_eq!(normalize_scan_count(Some(25)), 25);
        assert_eq!(normalize_scan_pattern(None), None);
        assert_eq!(normalize_scan_pattern(Some("".into())), None);
        assert_eq!(normalize_scan_pattern(Some("   ".into())), None);
        assert_eq!(
            normalize_scan_pattern(Some("user:*".into())),
            Some("user:*".into())
        );
        assert_eq!(
            normalize_scan_pattern(Some(" user:* ".into())),
            Some(" user:* ".into())
        );
    }

    #[test]
    fn filters_memory_results_by_min_size() {
        assert!(should_include_memory_result(4096, Some(1024)));
        assert!(should_include_memory_result(1024, Some(1024)));
        assert!(!should_include_memory_result(512, Some(1024)));
        assert!(should_include_memory_result(0, None));
        assert!(should_include_memory_result(0, Some(0)));
    }

    #[test]
    fn builds_memory_success_items_only_when_filter_and_limit_allow() {
        assert_eq!(
            memory_result_item_for_success("user:1".into(), 2048, Some(1024), 0, 10),
            Some(TaskEventItem::Memory {
                key: "user:1".into(),
                size: 2048
            })
        );
        assert_eq!(
            memory_result_item_for_success("user:1".into(), 512, Some(1024), 0, 10),
            None
        );
        assert_eq!(
            memory_result_item_for_success("user:1".into(), 2048, None, 10, 10),
            None
        );
    }

    #[test]
    fn builds_zero_size_memory_items_for_errors_without_positive_min_size() {
        assert_eq!(
            memory_result_item_for_error("user:1".into(), None, 0, 10),
            Some(TaskEventItem::Memory {
                key: "user:1".into(),
                size: 0
            })
        );
        assert_eq!(
            memory_result_item_for_error("user:1".into(), Some(0), 0, 10),
            Some(TaskEventItem::Memory {
                key: "user:1".into(),
                size: 0
            })
        );
        assert_eq!(
            memory_result_item_for_error("user:1".into(), Some(1), 0, 10),
            None
        );
        assert_eq!(
            memory_result_item_for_error("user:1".into(), None, 10, 10),
            None
        );
    }

    #[test]
    fn builds_scan_command_with_pattern_and_count() {
        let command = build_scan_command("42", Some("user:*"), 50);
        let packed = packed_command_string(&command);

        assert_ordered_tokens(&packed, &["SCAN", "42", "MATCH", "user:*", "COUNT", "50"]);
    }

    #[test]
    fn build_scan_command_ignores_blank_pattern_and_defaults_zero_count() {
        let command = build_scan_command("0", Some("   "), 0);
        let packed = packed_command_string(&command);

        assert!(packed.contains("SCAN"));
        assert!(packed.contains("2000"));
        assert!(!packed.contains("MATCH"));
    }

    #[test]
    fn chunks_delete_keys_in_bounded_batches() {
        let keys = vec!["a".into(), "b".into(), "c".into()];
        let chunks = chunk_keys(keys, 2);

        assert_eq!(
            chunks,
            vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["c".to_string()]
            ]
        );
    }

    /* ── 批量 TTL ─────────────────────────────────────── */

    fn relative_spec(seconds: i64) -> TtlSpec {
        TtlSpec {
            mode: TtlMode::Relative,
            value: Some(seconds),
            condition: None,
        }
    }

    #[test]
    fn ttl_reply_one_is_changed_zero_is_unchanged_and_error_is_failed() {
        // 0 既不是成功也不是失败：key 不存在、条件不满足、或 PERSIST 时本来就没 TTL。
        // 把它算进任何一边，界面上的数字就对不上了。
        assert_eq!(
            ttl_reply_outcome(Some(&redis::Value::Int(1))),
            TtlReplyOutcome::Changed
        );
        assert_eq!(
            ttl_reply_outcome(Some(&redis::Value::Int(0))),
            TtlReplyOutcome::Unchanged
        );
        // pipeline 回来的格子比 key 少：当失败，不要静默当成设上了
        assert_eq!(ttl_reply_outcome(None), TtlReplyOutcome::Failed);
        assert_eq!(
            ttl_reply_outcome(Some(&redis::Value::Nil)),
            TtlReplyOutcome::Failed
        );
        // 认不出的形状同样当失败
        assert_eq!(
            ttl_reply_outcome(Some(&redis::Value::Okay)),
            TtlReplyOutcome::Failed
        );
        // 被 ACL 拒掉的那一格是 `Value::ServerError`，它的内部表示是
        // crate 私有的、单元测试里构造不出来 —— 走的是与上面同一条
        // `optional_value → None` 的路。真实服务端的形状由 e2e 覆盖。
    }

    #[test]
    fn batch_ttl_progress_counts_unchanged_as_neither_success_nor_failure() {
        let mut progress = BatchTtlProgress::default();
        progress.record(TtlReplyOutcome::Changed);
        progress.record(TtlReplyOutcome::Changed);
        progress.record(TtlReplyOutcome::Unchanged);
        progress.record(TtlReplyOutcome::Failed);

        assert_eq!(progress.processed, 4);
        assert_eq!(progress.succeeded, 2);
        assert_eq!(progress.failed, 1);
        // 未变更由前端从这三个数算出来，见 doc/backend/tasks.md「进度口径」
        assert_eq!(progress.processed - progress.succeeded - progress.failed, 1);
    }

    #[test]
    fn batch_ttl_progress_adds_batches() {
        let mut total = BatchTtlProgress {
            processed: 3,
            succeeded: 2,
            failed: 1,
        };
        total.add(BatchTtlProgress {
            processed: 2,
            succeeded: 1,
            failed: 0,
        });

        assert_eq!(total.processed, 5);
        assert_eq!(total.succeeded, 3);
        assert_eq!(total.failed, 1);
    }

    #[test]
    fn validate_ttl_spec_rejects_the_shapes_that_mean_delete_now() {
        // EXPIRE key 0 立刻删掉 key
        assert_eq!(
            validate_ttl_spec(&relative_spec(0), 1_700_000_000)
                .unwrap_err()
                .payload()
                .detail
                .as_deref(),
            Some("A relative TTL must be at least one second")
        );
        assert!(validate_ttl_spec(&relative_spec(-5), 1_700_000_000).is_err());
        assert!(validate_ttl_spec(&relative_spec(1), 1_700_000_000).is_ok());

        // EXPIREAT 一个过去的时间戳同样是立刻删除
        let past = TtlSpec {
            mode: TtlMode::Absolute,
            value: Some(1_699_999_999),
            condition: None,
        };
        assert_eq!(
            validate_ttl_spec(&past, 1_700_000_000)
                .unwrap_err()
                .payload()
                .detail
                .as_deref(),
            Some("An absolute TTL must be in the future")
        );

        let now_exactly = TtlSpec {
            value: Some(1_700_000_000),
            ..past
        };
        assert!(validate_ttl_spec(&now_exactly, 1_700_000_000).is_err());
        let future = TtlSpec {
            value: Some(1_700_000_001),
            ..past
        };
        assert!(validate_ttl_spec(&future, 1_700_000_000).is_ok());
    }

    #[test]
    fn validate_ttl_spec_requires_a_value_unless_persisting() {
        let missing = TtlSpec {
            mode: TtlMode::Relative,
            value: None,
            condition: None,
        };
        assert!(validate_ttl_spec(&missing, 1_700_000_000).is_err());

        let persist = TtlSpec {
            mode: TtlMode::Persist,
            value: None,
            condition: None,
        };
        assert!(validate_ttl_spec(&persist, 1_700_000_000).is_ok());
    }

    #[test]
    fn validate_ttl_spec_rejects_a_condition_on_persist() {
        // PERSIST 没有条件 flag。界面上选「不过期」时条件区整块禁用，
        // 这里是同一条规则的后端一侧。
        let persist_with_condition = TtlSpec {
            mode: TtlMode::Persist,
            value: None,
            condition: Some(TtlCondition::Nx),
        };
        assert_eq!(
            validate_ttl_spec(&persist_with_condition, 1_700_000_000)
                .unwrap_err()
                .payload()
                .detail
                .as_deref(),
            Some("PERSIST does not take a condition")
        );
    }

    #[test]
    fn batch_pattern_is_used_verbatim_and_empty_is_dropped() {
        // 模式来自 matchPatternFor（已经是 `cache:page:*` 的形状），补就成了 `**`
        assert_eq!(
            batch_scan_pattern("  cache:page:*  "),
            Some("cache:page:*".to_string())
        );
        // 不补星号这条对删除更要紧：想精确删掉 `cache:page:home` 一个 key 的人，
        // 补完变成 `home*`，`home:v2` 与 `homepage` 会一起没。
        assert_eq!(
            batch_scan_pattern("cache:page:home"),
            Some("cache:page:home".to_string())
        );
        // 空模式会被 SCAN 当成"不带 MATCH" = 对全库动手。
        // 不该由一个没填的输入框触发。
        assert_eq!(batch_scan_pattern("   "), None);
        assert_eq!(batch_scan_pattern(""), None);
    }

    #[test]
    fn dry_run_registers_a_task_type_of_its_own() {
        // 状态栏显示「批量删除 正在运行」而实际只在数数，会让人以为自己删了库
        assert_eq!(
            batch_task_type("batch_delete", true),
            "batch_delete_dry_run"
        );
        assert_eq!(batch_task_type("batch_delete", false), "batch_delete");
        assert_eq!(batch_task_type("batch_ttl", true), "batch_ttl_dry_run");
        assert_eq!(batch_task_type("batch_ttl", false), "batch_ttl");
    }

    #[test]
    fn batch_delete_completed_message_only_speaks_for_the_dry_run() {
        let progress = BatchDeleteProgress {
            processed: 12_431,
            succeeded: 0,
            failed: 0,
        };
        assert_eq!(
            batch_delete_completed_message(progress, true).as_deref(),
            Some("Matched 12431 keys")
        );

        // 真跑靠计数说话，不必再来一句
        assert_eq!(
            batch_delete_completed_message(
                BatchDeleteProgress {
                    processed: 12_431,
                    succeeded: 12_400,
                    failed: 0,
                },
                false
            ),
            None
        );
    }

    #[test]
    fn batch_ttl_completed_message_spells_out_unchanged_keys() {
        let clean = BatchTtlProgress {
            processed: 10,
            succeeded: 10,
            failed: 0,
        };
        assert_eq!(batch_ttl_completed_message(clean, false), None);

        let mixed = BatchTtlProgress {
            processed: 100,
            succeeded: 87,
            failed: 2,
        };
        assert_eq!(
            batch_ttl_completed_message(mixed, false).as_deref(),
            Some("Set 87 keys, 11 unchanged, 2 failed")
        );

        let only_unchanged = BatchTtlProgress {
            processed: 5,
            succeeded: 3,
            failed: 0,
        };
        assert_eq!(
            batch_ttl_completed_message(only_unchanged, false).as_deref(),
            Some("Set 3 keys, 2 unchanged")
        );

        // 试算只报命中数 —— 它一个 key 都没改
        assert_eq!(
            batch_ttl_completed_message(
                BatchTtlProgress {
                    processed: 12_431,
                    succeeded: 0,
                    failed: 0,
                },
                true
            )
            .as_deref(),
            Some("Matched 12431 keys")
        );
    }

    #[tokio::test]
    async fn batch_ttl_completion_carries_the_summary_but_never_revives_a_failed_task() {
        let state = AppState::default();
        let started =
            register_task(&state, "conn-1".into(), "local".into(), "batch_ttl".into()).await;

        let snapshot = mark_batch_ttl_completed(
            &state,
            &started.task_id,
            Some("Set 3 keys, 1 unchanged".into()),
        )
        .await
        .unwrap();
        assert_eq!(snapshot.status, TaskStatus::Completed);
        assert_eq!(snapshot.message.as_deref(), Some("Set 3 keys, 1 unchanged"));

        let failed =
            register_task(&state, "conn-1".into(), "local".into(), "batch_ttl".into()).await;
        mark_task_failed(&state, &failed.task_id, "boom".into())
            .await
            .unwrap();
        let snapshot = mark_batch_ttl_completed(&state, &failed.task_id, Some("Set 3 keys".into()))
            .await
            .unwrap();
        assert_eq!(snapshot.status, TaskStatus::Failed);
        assert_eq!(snapshot.message.as_deref(), Some("boom"));
    }

    #[test]
    fn decodes_string_delete_args_and_rejects_binary_for_task_keys() {
        let key = redis_arg_to_string(&RedisCommandArg::String {
            value: "user:1".into(),
        })
        .unwrap();
        assert_eq!(key, "user:1");

        let err = redis_arg_to_string(&RedisCommandArg::Binary {
            base64: "AAE=".into(),
        })
        .unwrap_err();
        assert_eq!(err.payload().code, "redis.task_invalid_request");
    }

    #[test]
    fn records_successful_delete_chunk_counts_before_later_cancellation() {
        let mut progress = BatchDeleteProgress::default();

        record_batch_delete_success(&mut progress, 3, 2);

        assert_eq!(
            progress,
            BatchDeleteProgress {
                processed: 3,
                succeeded: 2,
                failed: 0,
            }
        );
    }

    #[test]
    fn honors_cancel_after_delete_only_when_more_work_remains() {
        assert!(!should_honor_cancel_after_delete(
            false, false, false, false
        ));
        assert!(should_honor_cancel_after_delete(true, false, false, false));
        assert!(should_honor_cancel_after_delete(false, true, false, false));
        assert!(should_honor_cancel_after_delete(false, false, true, false));
        assert!(should_honor_cancel_after_delete(false, false, false, true));
    }

    /// 有落点那条路发的是 5000 条独立的单 key `DEL`（同节点也不能合成一条，
    /// 跨槽位会被拒），请求体积与解析开销高一个量级，所以块要更小。
    #[test]
    fn splits_node_routed_delete_batches_into_smaller_chunks() {
        assert!(batch_delete_chunk_size(true) < batch_delete_chunk_size(false));
    }

    #[tokio::test]
    async fn batch_delete_success_progress_updates_counts_after_cancel_arrives() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "batch_delete".into(),
        )
        .await;
        cancel_task(&state, &started.task_id).await.unwrap();

        let snapshot = mark_task_counts(&state, &started.task_id, 3, 2, 0)
            .await
            .unwrap();

        assert_eq!(snapshot.status, TaskStatus::Cancelled);
        assert_eq!(snapshot.processed, 3);
        assert_eq!(snapshot.succeeded, 2);
        assert_eq!(snapshot.failed, 0);
    }

    #[tokio::test]
    async fn batch_delete_completion_can_complete_from_cancelled_after_work_is_done() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "batch_delete".into(),
        )
        .await;
        cancel_task(&state, &started.task_id).await.unwrap();

        let snapshot = mark_batch_delete_completed(&state, &started.task_id, None)
            .await
            .unwrap();

        assert_eq!(snapshot.status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn batch_delete_dry_run_completion_carries_the_match_count() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "batch_delete_dry_run".into(),
        )
        .await;

        let snapshot = mark_batch_delete_completed(
            &state,
            &started.task_id,
            Some("Matched 12431 keys".into()),
        )
        .await
        .unwrap();

        assert_eq!(snapshot.status, TaskStatus::Completed);
        assert_eq!(snapshot.message.as_deref(), Some("Matched 12431 keys"));
    }

    #[tokio::test]
    async fn batch_delete_completion_does_not_overwrite_failed() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "batch_delete".into(),
        )
        .await;
        mark_task_failed(&state, &started.task_id, "delete failed".into())
            .await
            .unwrap();

        let snapshot = mark_batch_delete_completed(&state, &started.task_id, None)
            .await
            .unwrap();

        assert_eq!(snapshot.status, TaskStatus::Failed);
        assert_eq!(snapshot.message.as_deref(), Some("delete failed"));
    }

    #[tokio::test]
    async fn memory_analysis_completion_can_complete_from_cancelled_after_work_is_done() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "memory_analysis".into(),
        )
        .await;
        cancel_task(&state, &started.task_id).await.unwrap();

        let snapshot = mark_memory_analysis_completed(&state, &started.task_id)
            .await
            .unwrap();

        assert_eq!(snapshot.status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn memory_analysis_completion_does_not_overwrite_failed() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "memory_analysis".into(),
        )
        .await;
        mark_task_failed(&state, &started.task_id, "memory failed".into())
            .await
            .unwrap();

        let snapshot = mark_memory_analysis_completed(&state, &started.task_id)
            .await
            .unwrap();

        assert_eq!(snapshot.status, TaskStatus::Failed);
        assert_eq!(snapshot.message.as_deref(), Some("memory failed"));
    }

    #[tokio::test]
    async fn memory_analysis_progress_updates_cancelled_after_key_completed_in_flight() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "memory_analysis".into(),
        )
        .await;
        cancel_task(&state, &started.task_id).await.unwrap();

        let snapshot = mark_task_counts(&state, &started.task_id, 1, 1, 0)
            .await
            .unwrap();
        let event = progress_event(
            &snapshot,
            vec![TaskEventItem::Memory {
                key: "user:1".into(),
                size: 2048,
            }],
            None,
        );

        assert_eq!(snapshot.status, TaskStatus::Cancelled);
        assert_eq!(snapshot.processed, 1);
        assert_eq!(snapshot.succeeded, 1);
        assert_eq!(snapshot.failed, 0);
        assert_eq!(
            event.items,
            vec![TaskEventItem::Memory {
                key: "user:1".into(),
                size: 2048
            }]
        );
    }

    #[tokio::test]
    async fn memory_analysis_progress_does_not_overwrite_failed() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "memory_analysis".into(),
        )
        .await;
        mark_task_failed(&state, &started.task_id, "memory failed".into())
            .await
            .unwrap();

        let snapshot = mark_task_counts(&state, &started.task_id, 1, 1, 0)
            .await
            .unwrap();

        assert_eq!(snapshot.status, TaskStatus::Failed);
        assert_eq!(snapshot.message.as_deref(), Some("memory failed"));
        assert_eq!(snapshot.processed, 0);
        assert_eq!(snapshot.succeeded, 0);
        assert_eq!(snapshot.failed, 0);
    }

    #[test]
    fn honors_cancel_after_memory_key_only_when_more_work_remains() {
        assert!(!should_honor_cancel_after_memory_key(false, false));
        assert!(should_honor_cancel_after_memory_key(true, false));
        assert!(should_honor_cancel_after_memory_key(false, true));
    }

    #[tokio::test]
    async fn failed_event_for_error_uses_normalized_app_error_message() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "batch_delete".into(),
        )
        .await;

        let event = failed_event_for_error(
            &state,
            &started.task_id,
            &AppError::RedisCommandFailed("ERR wrongtype".into()),
        )
        .await
        .unwrap();

        assert_eq!(event.status, TaskStatus::Failed);
        assert_eq!(event.message.as_deref(), Some("Redis command failed"));
    }

    fn packed_command_string(command: &redis::Cmd) -> String {
        String::from_utf8_lossy(&command.get_packed_command()).to_string()
    }

    fn assert_ordered_tokens(packed: &str, tokens: &[&str]) {
        let mut offset = 0;
        for token in tokens {
            let index = packed[offset..]
                .find(token)
                .unwrap_or_else(|| panic!("packed command missing token {token:?}: {packed:?}"));
            offset += index + token.len();
        }
    }

    /* ── 扫描进度的近似估算（doc/backend/tasks.md「扫描任务：只报命中数」）─────────────────── */

    #[test]
    fn reversed_cursor_fraction_maps_cursors_to_iteration_order() {
        // 表大小 16（位宽 4）：游标序列是 rev(t)，t = 1, 2, 3 → 8, 4, 12
        assert_eq!(reversed_cursor_fraction(8, 4), 1.0 / 16.0);
        assert_eq!(reversed_cursor_fraction(4, 4), 2.0 / 16.0);
        assert_eq!(reversed_cursor_fraction(12, 4), 3.0 / 16.0);
    }

    #[test]
    fn standalone_estimator_is_monotonic_over_a_cursor_sequence() {
        let mut estimator = ScanProgressEstimator::standalone();
        // 首页之前（以及解析不出的游标）尚无位宽信息：None，前端不显示
        assert_eq!(estimator.observe("not-a-cursor"), None);

        // 位宽 4 的完整前几步：rev(t)，t = 1..=5 → 8, 4, 12, 2, 10
        let mut last = 0.0;
        for cursor in ["8", "4", "12", "2", "10", "0"] {
            let estimate = estimator.observe(cursor).unwrap();
            assert!(
                estimate >= last,
                "estimate regressed at cursor {cursor}: {estimate} < {last}"
            );
            last = estimate;
        }
        assert_eq!(last, 1.0, "zero cursor should report completion");
    }

    #[test]
    fn standalone_estimator_does_not_regress_when_bit_width_grows() {
        let mut estimator = ScanProgressEstimator::standalone();
        // 位宽 3 的观测：4 = rev(1) → 1/8
        assert_eq!(estimator.observe("4"), Some(1.0 / 8.0));
        // rehash 扩表后冒出位宽 4 的游标：原始估算回落到 1/16，高水位不回退
        assert_eq!(estimator.observe("8"), Some(1.0 / 8.0));
        // 推进超过高水位后恢复正常上行
        assert_eq!(estimator.observe("12"), Some(3.0 / 16.0));
    }

    #[test]
    fn estimator_reports_completion_when_a_single_page_scans_everything() {
        let mut estimator = ScanProgressEstimator::standalone();
        assert_eq!(estimator.observe("0"), Some(1.0));
    }

    fn cluster_cursor(entries: &[(&str, &str)]) -> String {
        let map: std::collections::HashMap<String, String> = entries
            .iter()
            .map(|(endpoint, cursor)| (endpoint.to_string(), cursor.to_string()))
            .collect();
        // 复用 connection.rs 的唯一一份编码实现构造观测输入
        crate::services::connection::encode_cluster_scan_cursor(&map)
    }

    #[test]
    fn cluster_estimator_weights_nodes_by_dbsize_and_counts_finished_nodes_as_full() {
        let weights = std::collections::HashMap::from([
            ("a:7000".to_string(), 3000.0),
            ("b:7001".to_string(), 1000.0),
        ]);
        let mut estimator =
            ScanProgressEstimator::cluster(vec!["a:7000".into(), "b:7001".into()], Some(weights));

        // 两个节点都在 1/16 处：加权平均仍是 1/16
        let first = estimator
            .observe(&cluster_cursor(&[("a:7000", "8"), ("b:7001", "8")]))
            .unwrap();
        assert!((first - 1.0 / 16.0).abs() < 1e-9);

        // a 扫完（合成游标里缺席 = 游标归零）计 100%，b 推进到 2/16：
        // (3000 × 1 + 1000 × 0.125) / 4000
        let second = estimator
            .observe(&cluster_cursor(&[("b:7001", "4")]))
            .unwrap();
        assert!((second - 0.78125).abs() < 1e-9);

        // 全部归零时编码器给的就是 "0"
        assert_eq!(estimator.observe("0"), Some(1.0));
    }

    #[test]
    fn cluster_estimator_falls_back_to_equal_weights_without_dbsize() {
        let mut estimator =
            ScanProgressEstimator::cluster(vec!["a:7000".into(), "b:7001".into()], None);
        // a 扫完计 100%，b 在 1/16：等权平均 (1 + 0.0625) / 2
        let estimate = estimator
            .observe(&cluster_cursor(&[("b:7001", "8")]))
            .unwrap();
        assert!((estimate - 0.53125).abs() < 1e-9);
    }

    #[test]
    fn cluster_estimator_high_water_survives_topology_drift() {
        let mut estimator = ScanProgressEstimator::cluster(vec!["a:7000".into()], None);
        let first = estimator
            .observe(&cluster_cursor(&[("a:7000", "2")]))
            .unwrap();
        // 拓扑漂移：合成游标里冒出启动时没见过的节点，原始聚合值被拉低，
        // 对外的高水位不回退
        let second = estimator
            .observe(&cluster_cursor(&[("a:7000", "2"), ("c:7002", "8")]))
            .unwrap();
        assert!(
            second >= first,
            "aggregate estimate regressed: {second} < {first}"
        );
    }

    #[tokio::test]
    async fn scan_progress_lands_in_key_progress_events_and_snapshots() {
        let state = AppState::default();
        let started = register_task(
            &state,
            "conn-1".into(),
            "local".into(),
            "search_keys".into(),
        )
        .await;

        let mut tasks = state.tasks.lock().await;
        let record = tasks.get_mut(&started.task_id).unwrap();
        let event = progress_event_for_key_record(
            record,
            KeyProgressUpdate {
                processed: 1,
                succeeded: 1,
                failed: 0,
                total: None,
                estimated_progress: Some(0.25),
            },
            vec![],
        )
        .unwrap();

        assert_eq!(event.estimated_progress, Some(0.25));
        assert_eq!(super::snapshot(record).estimated_progress, Some(0.25));
    }
}
