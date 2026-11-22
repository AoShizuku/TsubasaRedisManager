use std::collections::HashMap;
use std::sync::{atomic::AtomicBool, Arc, OnceLock};
use tokio::sync::Mutex;

use crate::models::{ConnectionConfig, TaskStatus};
use crate::services::capabilities::{
    Capabilities, Capability, CapabilityChangeEvent, CapabilityId, CapabilityState,
};
use crate::services::config::ConnectionMode;
use crate::services::executor::RedisExecutor;

pub type RedisExecutorHandle = Arc<Mutex<RedisExecutor>>;
pub type RedisSessionRuntimeHandle = Arc<RedisSessionRuntime>;

pub enum SessionResource {
    SshTunnel(crate::services::ssh::SshTunnel),
}

pub struct RedisSessionRuntime {
    pub executor: RedisExecutorHandle,
    pub resources: Vec<SessionResource>,
    /// 建连时探测的能力，运行时可被降级（`capabilities::degrade`）。会话级，不跨会话缓存。
    /// 同步锁：临界区只有一次 map 读写，不跨 await。
    pub capabilities: std::sync::Mutex<Capabilities>,
}

impl RedisSessionRuntime {
    pub fn capability(&self, id: CapabilityId) -> Capability {
        self.capabilities
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
            .unwrap_or(Capability {
                state: CapabilityState::Unknown,
                usable: true,
                reason: String::new(),
            })
    }

    pub fn set_capability(&self, id: CapabilityId, capability: Capability) {
        self.capabilities
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, capability);
    }
}

/// 能力变化的出口。命令层在 `setup` 里注入 `app.emit`；无窗口的集成测试不注入，
/// 同 `clone_task::CloneEventSink` 的做法。
pub type CapabilitySink = Arc<dyn Fn(CapabilityChangeEvent) + Send + Sync>;

pub struct RedisSession {
    pub connection_name: String,
    pub readonly: bool,
    pub mode: ConnectionMode,
    pub config: ConnectionConfig,
    pub runtime: RedisSessionRuntimeHandle,
}

pub struct TaskRecord {
    /// 单 Key 克隆进入提交后不再受取消影响，必须报告实际写入结果。
    pub committing: bool,
    pub task_id: String,
    pub connection_id: String,
    pub connection_name: String,
    pub task_type: String,
    pub status: TaskStatus,
    pub processed: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub total: Option<u64>,
    /// 扫描类任务的近似进度（0..=1，`models::TaskStatusSnapshot` 的同名字段）。
    /// 只有 `run_scan_task` 写它，其余任务恒为 `None`。
    pub estimated_progress: Option<f64>,
    pub message: Option<String>,
    pub cancel: Arc<AtomicBool>,
    /// 暂停标志，与 `cancel` 同一模式（注册表持有、任务循环轮询）。
    ///
    /// **刻意不是一个 `TaskStatus`**：暂停服务的是「窗口隐藏时停手」，
    /// 用户根本不在看界面，不需要一个 UI 状态；加状态枚举会级联到前端
    /// 所有任务消费点。目前只有扫描任务在页间检查它。
    pub paused: Arc<AtomicBool>,
}

#[derive(Clone, Default)]
pub struct AppState {
    pub clone_tasks: Arc<Mutex<HashMap<String, crate::models::CloneTaskSnapshot>>>,
    pub sessions: Arc<Mutex<HashMap<String, RedisSession>>>,
    pub tasks: Arc<Mutex<HashMap<String, TaskRecord>>>,
    pub capability_sink: Arc<OnceLock<CapabilitySink>>,
}

impl AppState {
    pub fn clone_for_task(&self) -> Self {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use crate::models::ConnectionConfig;

    use super::{RedisExecutorHandle, RedisSessionRuntime, RedisSessionRuntimeHandle};

    #[test]
    fn redis_session_runtime_handles_are_cloneable_and_own_resources() {
        fn assert_clone<T: Clone>() {}
        fn assert_runtime_fields(runtime: &RedisSessionRuntime) {
            let _: &RedisExecutorHandle = &runtime.executor;
            let _: &Vec<super::SessionResource> = &runtime.resources;
        }

        assert_clone::<RedisSessionRuntimeHandle>();
        let _ = assert_runtime_fields;
    }

    #[test]
    fn redis_session_exposes_config_for_dedicated_tasks() {
        fn assert_config_field(session: &super::RedisSession) -> &ConnectionConfig {
            &session.config
        }

        let _ = assert_config_field;
    }
}
