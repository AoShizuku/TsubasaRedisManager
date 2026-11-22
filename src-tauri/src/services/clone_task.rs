//! 单 Key 克隆：有界后台传输、独占连接、目标时钟校准与不可取消的提交阶段。
use crate::error::{AppError, AppResult};
use crate::models::{
    ClonePhase, CloneTaskSnapshot, ConnectionConfig, TaskEventItem, TaskStarted, TaskStatus,
};
use crate::services::capabilities::{self, CapabilityId};
use crate::services::executor::PipelineTarget;
use crate::services::key_write::{
    fault, map_error, same_server, KeyExpiry, KeyWriteResult, MAX_DUMP_BYTES, RESTORE_SCRIPT,
    SNAPSHOT_SCRIPT,
};
use crate::services::{connection, task};
use crate::state::AppState;
use std::future::Future;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

// DUMP 不可流式读取；全应用只允许一份在途 payload，避免并发复制撑爆内存。
static TRANSFER_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
// 事件注入留在命令层，使无窗口集成测试不链接 WebView 的窗口消息入口。
pub type CloneEventSink = Arc<dyn Fn(crate::models::TaskProgressEvent) + Send + Sync>;
const STAGE_TIMEOUT: Duration = Duration::from_secs(300);

struct Run {
    state: AppState,
    app: Option<CloneEventSink>,
    task_id: Option<String>,
    cancel: Arc<AtomicBool>,
    source: ConnectionConfig,
    target: ConnectionConfig,
    source_key: String,
    target_key: String,
    expiry: KeyExpiry,
}

impl Run {
    async fn report(
        &self,
        phase: ClonePhase,
        bytes: Option<u64>,
        key_type: Option<String>,
        error: Option<&AppError>,
        message: &str,
    ) {
        let Some(id) = &self.task_id else { return };
        let detail = {
            let mut details = self.state.clone_tasks.lock().await;
            let Some(detail) = details.get_mut(id) else {
                return;
            };
            detail.phase = phase;
            if bytes.is_some() {
                detail.bytes = bytes;
            }
            if key_type.is_some() {
                detail.key_type = key_type;
            }
            detail.error_code = error.map(|e| e.payload().code);
            detail.message = message.into();
            detail.clone()
        };
        {
            let mut records = self.state.tasks.lock().await;
            if let Some(record) = records.get_mut(id) {
                record.status = match phase {
                    ClonePhase::Completed => TaskStatus::Completed,
                    ClonePhase::Cancelled => TaskStatus::Cancelled,
                    ClonePhase::Failed | ClonePhase::Uncertain => TaskStatus::Failed,
                    _ => TaskStatus::Progress,
                };
                record.total = Some(1);
                record.processed = u64::from(matches!(
                    phase,
                    ClonePhase::Completed | ClonePhase::Failed | ClonePhase::Uncertain
                ));
                record.succeeded = u64::from(phase == ClonePhase::Completed);
                record.failed =
                    u64::from(matches!(phase, ClonePhase::Failed | ClonePhase::Uncertain));
                record.message = Some(message.into());
            }
        }
        if let (Some(app), Ok(snapshot)) = (&self.app, task::get_task_status(&self.state, id).await)
        {
            app(task::progress_event(
                &snapshot,
                vec![TaskEventItem::Clone { snapshot: detail }],
                Some(message.into()),
            ));
        }
    }

    async fn cancellable<T>(&self, future: impl Future<Output = AppResult<T>>) -> AppResult<T> {
        tokio::select! {
            result = tokio::time::timeout(STAGE_TIMEOUT, future) => result.map_err(|_| fault("read_timeout", "读取或准备阶段超时，目标尚未写入"))?,
            _ = async { loop {
                if self.cancel.load(Ordering::SeqCst) { break; }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }} => Err(AppError::TaskCancelled("目标尚未写入".into())),
        }
    }

    async fn commit(&self) -> AppResult<()> {
        if let Some(id) = &self.task_id {
            // 与 cancel_task 共用同一把锁，取消获准就不可能再进入目标写入。
            let mut records = self.state.tasks.lock().await;
            let record = records
                .get_mut(id)
                .ok_or_else(|| AppError::TaskNotFound(id.clone()))?;
            if record.cancel.load(Ordering::SeqCst) {
                return Err(AppError::TaskCancelled(id.clone()));
            }
            record.committing = true;
        } else if self.cancel.load(Ordering::SeqCst) {
            return Err(AppError::TaskCancelled("目标尚未写入".into()));
        }
        self.report(
            ClonePhase::Restoring,
            None,
            None,
            None,
            "正在写入目标，等待服务器确认",
        )
        .await;
        Ok(())
    }

    async fn transfer(&self) -> AppResult<KeyWriteResult> {
        let _permit = self
            .cancellable(async {
                TRANSFER_SLOT
                    .acquire()
                    .await
                    .map_err(|_| fault("unavailable", "传输队列已关闭"))
            })
            .await?;
        self.report(
            ClonePhase::Connecting,
            None,
            None,
            None,
            "正在连接并检查目标",
        )
        .await;
        let (source, target) = self
            .cancellable(async {
                tokio::try_join!(
                    connection::open_transfer_runtime(&self.source),
                    connection::open_transfer_runtime(&self.target)
                )
            })
            .await?;
        // ACL 拒绝 COMMAND INFO 只意味着未知，最终由目标校验真实 payload。
        let mut probe = redis::pipe();
        probe.cmd("COMMAND").arg("INFO").arg("RESTORE");
        let probe = self
            .cancellable(async {
                target
                    .executor
                    .lock()
                    .await
                    .query_pipeline_raw(probe, PipelineTarget::Key(&self.target_key))
                    .await
            })
            .await?;
        if matches!(probe.first(), Some(redis::Value::Array(values)) if matches!(values.first(), Some(redis::Value::Nil)))
        {
            return Err(fault(
                "unsupported",
                "目标服务器不支持 RESTORE，无法原样克隆",
            ));
        }
        let mut clock = redis::cmd("EVAL");
        clock.arg("if redis.call('EXISTS', KEYS[1]) ~= 0 then return redis.error_reply('KEY_EXISTS') end return redis.call('TIME')").arg(1).arg(&self.target_key);
        let clock_reply = self
            .cancellable(async {
                target
                    .executor
                    .lock()
                    .await
                    .query_raw(clock)
                    .await
                    .map_err(map_error)
            })
            .await?;
        let (seconds, micros): (i64, i64) = redis::from_redis_value(clock_reply)
            .map_err(|e| AppError::RedisCommandFailed(e.to_string()))?;
        let target_clock = seconds * 1000 + micros / 1000;
        let target_received = Instant::now();
        self.report(ClonePhase::Reading, None, None, None, "正在读取完整快照")
            .await;
        let mut command = redis::cmd("EVAL");
        command
            .arg(SNAPSHOT_SCRIPT)
            .arg(1)
            .arg(&self.source_key)
            .arg(MAX_DUMP_BYTES);
        let source_started = Instant::now();
        let reply = self
            .cancellable(async {
                source
                    .executor
                    .lock()
                    .await
                    .query_raw(command)
                    .await
                    .map_err(map_error)
            })
            .await?;
        let (key_type, dump, expires_at, ttl): (String, Vec<u8>, String, i64) =
            redis::from_redis_value(reply)
                .map_err(|e| AppError::RedisCommandFailed(e.to_string()))?;
        let (mode, milliseconds) = self.expiry.args(true)?;
        let preserved = if same_server(&self.source, &self.target) {
            expires_at
        } else {
            translated_deadline(
                target_clock,
                source_started.duration_since(target_received),
                ttl,
            )?
            .to_string()
        };
        self.report(
            ClonePhase::Reading,
            Some(dump.len() as u64),
            Some(key_type.clone()),
            None,
            "快照已读取，准备写入目标",
        )
        .await;
        let mut restore = redis::cmd("EVAL");
        restore
            .arg(RESTORE_SCRIPT)
            .arg(1)
            .arg(&self.target_key)
            .arg(dump)
            .arg(mode)
            .arg(milliseconds)
            .arg(preserved);
        self.commit().await?;
        // 提交后不 select 取消：网络超时不等于未写入，不自动重试。
        let result = tokio::time::timeout(STAGE_TIMEOUT, async {
            target
                .executor
                .lock()
                .await
                .query_raw_detailed(restore)
                .await
        })
        .await;
        match result {
            Err(_) => {
                return Err(fault(
                    "outcome_unknown",
                    "目标写入响应超时，结果待确认；请检查目标 Key",
                ))
            }
            Ok(Err(error)) => return Err(restore_error(error)),
            Ok(Ok(_)) => {}
        }
        Ok(KeyWriteResult { key_type })
    }
}

/// 目标 TIME 接收时刻和源请求发出时刻均取保守下界。固定的目标绝对期限使
/// 下载、排队、上传耗时自然消耗 TTL，不依赖本机墙钟，也不重新开始倒计时。
fn translated_deadline(target_clock: i64, before_source: Duration, ttl: i64) -> AppResult<i64> {
    match ttl {
        -1 => Ok(0),
        0.. => Ok(target_clock + before_source.as_millis() as i64 + ttl),
        _ => Err(fault("source_missing", "源 Key 已不存在")),
    }
}

fn restore_error(error: redis::RedisError) -> AppError {
    if error.is_io_error() || error.is_timeout() || error.is_connection_dropped() {
        return fault(
            "outcome_unknown",
            "目标写入连接中断或超时，结果待确认；请检查目标 Key",
        );
    }
    let message = error.to_string();
    if message.contains("DUMP payload")
        || message.contains("Bad data format")
        || message.contains("module data")
    {
        return fault(
            "incompatible",
            "目标无法恢复该序列化格式，请检查 Redis 版本或所需模块；未进行有损转换",
        );
    }
    map_error(AppError::from_redis("EVAL", &error))
}

async fn prepare(
    state: &AppState,
    source_id: &str,
    target_id: &str,
    source_key: &str,
    target_key: &str,
    expiry: KeyExpiry,
) -> AppResult<Run> {
    expiry.args(true)?;
    let sessions = state.sessions.lock().await;
    let source = sessions
        .get(source_id)
        .ok_or_else(|| AppError::ConnectionNotFound(source_id.into()))?;
    let target = sessions
        .get(target_id)
        .ok_or_else(|| AppError::ConnectionNotFound(target_id.into()))?;
    if target.readonly {
        return Err(AppError::ReadonlyWriteRejected("RESTORE".into()));
    }
    // 快照与恢复都是脚本。查的是用户的浏览会话 —— 传输走独立的 transfer runtime，
    // 它的能力表随用随弃，在那上面降级不会被界面看见。
    capabilities::require(&source.runtime, CapabilityId::Scripting)?;
    capabilities::require(&target.runtime, CapabilityId::Scripting)?;
    if same_server(&source.config, &target.config)
        && source.config.db.unwrap_or(0) == target.config.db.unwrap_or(0)
        && source_key == target_key
    {
        return Err(fault("invalid_target", "源 Key 与目标 Key 相同"));
    }
    Ok(Run {
        state: state.clone(),
        app: None,
        task_id: None,
        cancel: Arc::new(AtomicBool::new(false)),
        source: source.config.clone(),
        target: target.config.clone(),
        source_key: source_key.into(),
        target_key: target_key.into(),
        expiry,
    })
}

pub async fn get_clone_task(state: &AppState, task_id: &str) -> AppResult<CloneTaskSnapshot> {
    state
        .clone_tasks
        .lock()
        .await
        .get(task_id)
        .cloned()
        .ok_or_else(|| AppError::TaskNotFound(task_id.into()))
}

pub async fn start_clone_task(
    app: Option<CloneEventSink>,
    state: &AppState,
    source_id: &str,
    target_id: &str,
    source_key: &str,
    target_key: &str,
    expiry: KeyExpiry,
) -> AppResult<TaskStarted> {
    let mut run = prepare(state, source_id, target_id, source_key, target_key, expiry).await?;
    let source_name = format!(
        "{} / db{}",
        task::require_connection_name(state, source_id).await?,
        run.source.db.unwrap_or(0)
    );
    let target_name = format!(
        "{} / db{}",
        task::require_connection_name(state, target_id).await?,
        run.target.db.unwrap_or(0)
    );
    let snapshot = task::register_task(
        state,
        target_id.into(),
        target_name.clone(),
        "clone_key".into(),
    )
    .await;
    let id = snapshot.task_id;
    state.clone_tasks.lock().await.insert(
        id.clone(),
        CloneTaskSnapshot {
            task_id: id.clone(),
            source_connection_id: source_id.into(),
            target_connection_id: target_id.into(),
            source_name,
            target_name,
            source_key: source_key.into(),
            target_key: target_key.into(),
            phase: ClonePhase::Queued,
            bytes: None,
            key_type: None,
            error_code: None,
            message: "等待传输队列".into(),
        },
    );
    run.app = app;
    run.cancel = task::task_cancel_token(state, &id).await?;
    run.task_id = Some(id.clone());
    run.report(ClonePhase::Queued, None, None, None, "等待传输队列")
        .await;
    tokio::spawn(async move {
        match run.transfer().await {
            Ok(result) => {
                run.report(
                    ClonePhase::Completed,
                    None,
                    Some(result.key_type),
                    None,
                    "已克隆 Key",
                )
                .await
            }
            Err(error) => {
                let payload = error.payload();
                let phase = if matches!(error, AppError::TaskCancelled(_)) {
                    ClonePhase::Cancelled
                } else if payload.code == "redis.key_outcome_unknown" {
                    ClonePhase::Uncertain
                } else {
                    ClonePhase::Failed
                };
                let message = if phase == ClonePhase::Cancelled {
                    "已取消克隆，目标未写入".into()
                } else {
                    payload
                        .detail
                        .map(|detail| format!("{}：{detail}", payload.message))
                        .unwrap_or(payload.message)
                };
                run.report(phase, None, None, Some(&error), &message).await;
            }
        }
    });
    Ok(TaskStarted { task_id: id })
}

pub(crate) async fn clone_inline(
    state: &AppState,
    source_id: &str,
    target_id: &str,
    source_key: &str,
    target_key: &str,
    expiry: KeyExpiry,
) -> AppResult<KeyWriteResult> {
    prepare(state, source_id, target_id, source_key, target_key, expiry)
        .await?
        .transfer()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn target_clock_translation_keeps_a_fixed_deadline() {
        let deadline = translated_deadline(10_000, Duration::from_millis(500), 2000).unwrap();
        assert_eq!(deadline, 12_500);
        assert_eq!(deadline - 11_000, 1500);
        assert_eq!(
            translated_deadline(99_000_000, Duration::from_secs(2), -1).unwrap(),
            0
        );
        assert!(translated_deadline(100, Duration::ZERO, -2).is_err());
    }
    #[tokio::test]
    async fn cancellation_does_not_claim_to_undo_committed_writes() {
        let state = AppState::default();
        let id = task::register_task(&state, "target".into(), "target".into(), "clone_key".into())
            .await
            .task_id;
        task::cancel_task(&state, &id).await.unwrap();
        assert!(state.tasks.lock().await[&id].cancel.load(Ordering::SeqCst));
        assert_ne!(state.tasks.lock().await[&id].status, TaskStatus::Cancelled);
        let config: ConnectionConfig =
            serde_json::from_str(r#"{"host":"127.0.0.1","port":6379}"#).unwrap();
        let mut run = Run {
            state: state.clone(),
            app: None,
            task_id: Some(id.clone()),
            cancel: task::task_cancel_token(&state, &id).await.unwrap(),
            source: config.clone(),
            target: config,
            source_key: "source".into(),
            target_key: "target".into(),
            expiry: KeyExpiry::Persist,
        };
        assert!(run.commit().await.is_err());
        let id = task::register_task(&state, "target".into(), "target".into(), "clone_key".into())
            .await
            .task_id;
        run.task_id = Some(id.clone());
        run.cancel = task::task_cancel_token(&state, &id).await.unwrap();
        run.commit().await.unwrap();
        assert!(task::cancel_task(&state, &id).await.is_err());
        assert!(!state.tasks.lock().await[&id].cancel.load(Ordering::SeqCst));
    }
    #[test]
    fn a_write_timeout_is_reported_as_unknown_not_failed_or_cancelled() {
        let error = redis::RedisError::from(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "test response lost",
        ));
        assert_eq!(
            restore_error(error).payload().code,
            "redis.key_outcome_unknown"
        );
    }
}
