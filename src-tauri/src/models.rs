use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub auth: Option<String>,
    pub username: Option<String>,
    pub db: Option<i64>,
    pub name: Option<String>,
    pub separator: Option<String>,
    pub connection_read_only: Option<bool>,
    pub cluster: Option<bool>,
    pub ssl_options: Option<SslOptions>,
    pub ssh_options: Option<SshOptions>,
    pub sentinel_options: Option<SentinelOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SslOptions {
    pub key: Option<String>,
    pub cert: Option<String>,
    pub ca: Option<String>,
    pub servername: Option<String>,
    pub insecure: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SshOptions {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub privatekey: Option<String>,
    pub privatekeybookmark: Option<String>,
    pub passphrase: Option<String>,
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SentinelOptions {
    pub master_name: String,
    pub node_password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSummary {
    pub key: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub ssh_host: Option<String>,
    pub ssh_port: Option<u16>,
    pub ssh_username: Option<String>,
    pub sentinel_master_name: Option<String>,
    pub order: Option<i64>,
    pub color: Option<String>,
    pub cluster: bool,
    pub readonly: bool,
    pub ssh_enabled: bool,
    pub tls_enabled: bool,
    pub sentinel_enabled: bool,
    pub has_auth: bool,
    pub has_ssh_password: bool,
    pub has_ssh_passphrase: bool,
    pub has_sentinel_node_password: bool,
    pub has_tls_key: bool,
    pub has_tls_cert: bool,
    pub has_tls_ca: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionEditDraft {
    pub key: Option<String>,
    pub name: Option<String>,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub db: Option<i64>,
    pub separator: Option<String>,
    pub connection_read_only: Option<bool>,
    pub cluster: Option<bool>,
    pub color: Option<String>,
    pub order: Option<i64>,
    pub auth: Option<String>,
    pub ssl_options: Option<SslOptions>,
    pub ssh_options: Option<ConnectionStoreSshDraft>,
    pub sentinel_options: Option<ConnectionStoreSentinelDraft>,
    pub has_auth: bool,
    pub has_ssh_password: bool,
    pub has_ssh_passphrase: bool,
    pub has_sentinel_node_password: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSaveDraft {
    pub key: Option<String>,
    pub name: Option<String>,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub db: Option<i64>,
    pub separator: Option<String>,
    pub connection_read_only: Option<bool>,
    pub cluster: Option<bool>,
    pub color: Option<String>,
    pub order: Option<i64>,
    pub auth: Option<String>,
    pub clear_auth: Option<bool>,
    pub ssl_options: Option<SslOptions>,
    pub ssh_options: Option<ConnectionStoreSshDraft>,
    pub sentinel_options: Option<ConnectionStoreSentinelDraft>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStoreSshDraft {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub clear_password: Option<bool>,
    pub privatekey: Option<String>,
    pub privatekeybookmark: Option<String>,
    pub passphrase: Option<String>,
    pub clear_passphrase: Option<bool>,
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStoreSentinelDraft {
    pub master_name: String,
    pub node_password: Option<String>,
    pub clear_node_password: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionMigrationResult {
    pub migrated: Vec<String>,
    #[serde(default)]
    pub migrated_indexes: Vec<usize>,
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionOpened {
    pub connection_id: String,
    pub connection_name: String,
    /// 这个会话的只读状态，与 `writable_runtime` 判的是同一个值。
    /// 前端据此禁用写入控件 —— 不要让它再从连接配置自己推断一遍，
    /// 那是两套判断，会话建立后配置改了就会漂移。
    pub readonly: bool,
    /// 这是什么实现、什么版本。界面展示一律用它（「Valkey 8.1.1」），不要写 `redis_version`。
    pub server: crate::services::capabilities::ServerIdentity,
    /// 建连时的能力判定。会话内可能被降级，变化经 `redis://capability-change` 推送。
    /// 见 doc/backend/capabilities.md。
    pub capabilities: crate::services::capabilities::Capabilities,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub command: String,
    pub value: RedisValue,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandLogEvent {
    pub connection_id: String,
    pub connection_name: String,
    pub command: String,
    pub args: String,
    pub elapsed_ms: u128,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStatusEvent {
    pub connection_id: Option<String>,
    pub connection_key: Option<String>,
    pub connection_name: String,
    pub status: String,
    pub code: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisCommandRequest {
    pub command: String,
    pub args: Vec<RedisCommandArg>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RedisCommandArg {
    String { value: String },
    Binary { base64: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyListResult {
    pub keys: Vec<KeyEntry>,
    pub cursor: String,
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyEntry {
    pub key: String,
    /// SCAN 本身不带类型，由后端在同一次往返里补一批 `TYPE`。
    /// key 在 SCAN 与 TYPE 之间被删掉时为 `None`。
    pub key_type: Option<String>,
    /// 只在请求方要了才有（每 key 多一条 `TTL`，成本翻倍）。
    /// 对应设置项「外观 → Key 树 → 显示 TTL 标记」。
    pub ttl: Option<i64>,
}

/// Key 元信息头需要的全部字段，一次拿回。
///
/// v0.1 的前端为此串行发 4~6 条命令（`useKeyMeta.ts`），每打开一个标签页跑一遍。
/// 这里两批 pipeline 就够：TYPE 决定了用哪个大小命令，所以必须先拿到它。
///
/// `size` / `encoding` / `memory` / `slot` 都是 `Option` —— 命令可能被 ACL 或
/// `rename-command` 拒掉、槽位只有集群才有。用类型表达"可能没有"，
/// 而不是让前端拿 try/catch 去兜。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyMeta {
    pub key: String,
    /// key 不存在时为 `None`（`TYPE` 返回 `none`）。
    pub key_type: Option<String>,
    /// `-1` 永不过期，`-2` key 不存在，其余为剩余秒数。
    pub ttl: i64,
    /// 元素数量。STRING 是字节数，其余是成员数。
    pub size: Option<u64>,
    pub encoding: Option<String>,
    pub memory: Option<u64>,
    /// 只有集群才有槽位概念，单机下这一格不显示而不是显示 0。
    pub slot: Option<u16>,
    /// string 且带合法 `HYLL` 头时为 `Some` —— HyperLogLog 是 string 的
    /// **子形态**而不是第七种类型（`TYPE` 就返回 string），`key_type` 不动，
    /// 前端据此换用 HLL 查看器。识别只发生在打开 key 时，SCAN 不做。
    pub hll: Option<HllMeta>,
}

/// HLL 子形态的元信息。见 `services/hll.rs` 与 `doc/backend/hll.md`。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HllMeta {
    /// "dense" | "sparse"，取自头部第 5 字节。
    pub encoding: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum RedisValue {
    Null,
    String(String),
    Integer(i64),
    Array(Vec<RedisValue>),
    Object(serde_json::Value),
}

/// 值的统一形状，语义化取值命令一律用它。
///
/// `RedisValue` 里 BulkString 是 UTF-8 就转 `String`、不是就转
/// `Object { type: "binary" }` —— 同一个字段两种形状，前端每处都要分支。
/// 而 gzip 的魔数是 `1f 8b`，压缩值必然非 UTF-8，所以那不是边缘情况。
/// 详见 `doc/backend/ipc-contract.md`「RedisText」。
///
/// `text` 与 `base64` 恰好有一个是 `Some`：字节是合法 UTF-8 时只给 `text`
/// （UTF-8 可无损还原字节，不必每个值都多传一份 base64），否则只给 `base64`。
/// 前端一律 `text ?? base64`，只在展示层分支，不在结构上分支。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RedisText {
    pub text: Option<String>,
    pub base64: Option<String>,
    /// 被截断时的原始总字节数，未截断为 `None`。
    /// 界面必须如实写出"显示前 N / 共 M"，不要默默给一个看起来完整的值。
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Started,
    Progress,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TaskStarted {
    pub task_id: String,
}

// 不是 Eq：estimated_progress 是 f64。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusSnapshot {
    pub task_id: String,
    pub connection_id: String,
    pub connection_name: String,
    pub task_type: String,
    pub status: TaskStatus,
    pub processed: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub total: Option<u64>,
    /// 扫描类任务（`search_keys` / `load_all_keys`）的近似进度，**0..=1**。
    /// 由 SCAN 游标的反转二进制估算（`task::ScanProgressEstimator`，
    /// `doc/backend/tasks.md`「扫描任务：只报命中数」），任务内单调不减；其余任务类型恒为
    /// `None`，前端据此不显示百分比。**显示时必须带「约」字** ——
    /// rehash 抖动、DBSIZE 脱节都会让它失真，这是文档的硬要求。
    pub estimated_progress: Option<f64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgressEvent {
    pub task_id: String,
    pub connection_id: String,
    pub connection_name: String,
    pub task_type: String,
    pub status: TaskStatus,
    pub processed: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub total: Option<u64>,
    /// 同 `TaskStatusSnapshot::estimated_progress`：0..=1、只有扫描类任务有、
    /// 单调不减、显示必须带「约」。
    pub estimated_progress: Option<f64>,
    pub items: Vec<TaskEventItem>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TaskEventItem {
    Clone {
        snapshot: CloneTaskSnapshot,
    },
    #[serde(rename_all = "camelCase")]
    Key {
        key: String,
        /// SCAN 本身不带类型，扫描任务逐页 pipeline 一批 `TYPE` 补上 ——
        /// 与取值路径 `redis_scan_keys` 的 `KeyEntry` 同一契约（树的类型点
        /// 依赖它，见 `doc/backend/tasks.md`「事件项带 keyType」）。
        /// key 在 SCAN 与 TYPE 之间被删掉、或 `TYPE` 被 ACL 挡掉时为 `None`。
        key_type: Option<String>,
        /// 只在任务选项开了 `withTtl` 才有；`-1` 永不过期。
        ttl: Option<i64>,
    },
    Memory {
        key: String,
        size: u64,
    },
    Monitor {
        timestamp: String,
        database: Option<i64>,
        source: Option<String>,
        args: Vec<String>,
        raw: String,
    },
    PubSubMessage {
        channel: String,
        pattern: Option<String>,
        message: String,
    },
    File {
        path: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ClonePhase {
    Queued,
    Connecting,
    Reading,
    Restoring,
    Completed,
    Cancelled,
    Failed,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CloneTaskSnapshot {
    pub task_id: String,
    pub source_connection_id: String,
    pub target_connection_id: String,
    pub source_name: String,
    pub target_name: String,
    pub source_key: String,
    pub target_key: String,
    pub phase: ClonePhase,
    pub bytes: Option<u64>,
    pub key_type: Option<String>,
    pub error_code: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyScanTaskOptions {
    pub pattern: Option<String>,
    pub count: Option<usize>,
    /// 与取值路径 `redis_scan_keys` 的 `withTtl` 同义：每 key 再补一条 `TTL`，
    /// 成本翻倍，由前端按「外观 → Key 树 → 显示 TTL 标记」决定。缺省不取。
    pub with_ttl: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteTaskOptions {
    pub keys: Vec<RedisCommandArg>,
    pub patterns: Vec<String>,
    /// 只扫不删，用于试算。与 `BatchTtlTaskOptions::dry_run` 同构。
    ///
    /// 模式的命中数在 SCAN 之前谁也不知道，而删除是不可撤销的 ——
    /// 界面上出现的每个数量都必须是真的，所以试算走**完全相同**的
    /// 模式解析、扫描、分块路径，只是不发 `DEL`、只累加计数。
    /// 见 `doc/backend/tasks.md`「试算」。
    pub dry_run: Option<bool>,
    pub count: Option<usize>,
}

/// TTL 的三种模式，与 `doc/frontend/ttl.md`「三个模式直接对应三条命令」的 popover 一一对应。
///
/// 收敛成一个参数是因为它们是**同一个用户动作**的三种表达，而不是三件不同的事
/// —— 这也是包装厚度的上限，见 `doc/backend/ipc-contract.md`「一个用户动作一个命令」。
/// 单 key（`redis_set_key_ttl`）与批量（`batch_ttl` 任务）共用它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TtlMode {
    /// `PERSIST key`
    Persist,
    /// `EXPIRE key <seconds>`
    Relative,
    /// `EXPIREAT key <unix-timestamp>`
    Absolute,
}

/// `EXPIRE` / `EXPIREAT` 的条件 flag。**Redis 7.0+**。
///
/// 单 key 编辑用不上，批量必须有：没有条件的批量 TTL 会把范围内已经设好的 TTL
/// 全部覆盖掉 —— 给 `cache:*` 补 TTL，会把里面本来设了 7 天的 key 全改成 1 天。
/// 见 `doc/frontend/batch.md`「条件 flag」。
///
/// 低版本服务端收到这些 flag 会直接报错，所以前端按能力 `expireConditions`
/// 决定是否放出这组选项，而不是等报错；`start_batch_ttl_task` 守同一条规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TtlCondition {
    /// 只给还没有 TTL 的 key 设 —— 补上漏设的那些
    Nx,
    /// 只给已经有 TTL 的 key 设
    Xx,
    /// 只在会延长时设 —— 续命，不误伤本来设得更长的
    Gt,
    /// 只在会缩短时设
    Lt,
}

/// 批量设置 TTL。
///
/// `keys` 与 `patterns` 对应界面上的两种作用范围（勾选的显式列表 / 匹配模式），
/// 与 `BatchDeleteTaskOptions` 同构。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchTtlTaskOptions {
    pub keys: Vec<RedisCommandArg>,
    pub patterns: Vec<String>,
    pub mode: TtlMode,
    /// `relative` 是秒数、`absolute` 是 unix 时间戳；`persist` 时为 `None`。
    pub value: Option<i64>,
    pub condition: Option<TtlCondition>,
    /// 只扫不改，用于试算。
    ///
    /// 走**完全相同**的模式解析、扫描、分块路径，只是不发写命令、只累加计数。
    /// 用另一条扫描路径去数一遍是错的：试算的全部价值就在于
    /// 「试算出来的数字就是真会改的数字」。见 `doc/backend/tasks.md`「试算」。
    pub dry_run: Option<bool>,
    pub count: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryAnalysisTaskOptions {
    pub pattern: Option<String>,
    pub count: Option<usize>,
    pub min_size: Option<u64>,
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionTaskOptions {
    pub kind: SubscriptionKind,
    pub channels: Vec<String>,
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SubscriptionKind {
    Subscribe,
    Psubscribe,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportKeysTaskOptions {
    pub keys: Vec<RedisCommandArg>,
    pub output_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    pub is_maximized: bool,
    pub is_minimized: bool,
    pub is_visible: bool,
    pub scale_factor: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckOptions {
    pub endpoint: Option<String>,
    pub current_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UpdateCheckStatus {
    Available,
    NotAvailable,
    Failed,
    NotConfigured,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    pub status: UpdateCheckStatus,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub release_url: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProtoFile {
    pub path: String,
    pub name: String,
    pub content: String,
}
