/**
 * Rust 侧数据结构的 TypeScript 镜像。
 *
 * 所有 `#[serde(rename_all = "camelCase")]` 的结构在这里都是 camelCase。
 * 改动 src-tauri/src/models.rs 时必须同步改这里 —— Tauri 不生成绑定，
 * 这份文件就是唯一的契约声明。
 */

/* ── 错误 ─────────────────────────────────────────────────
   AppError 通过 Serialize 落到前端时就是 AppErrorPayload。
   code 是稳定标识（如 redis.readonly_write_rejected），
   message 面向用户，detail 是原始错误串。 */
export interface AppErrorPayload {
  code: string
  message: string
  detail: string | null
  recoverable: boolean
}

/* ── 连接配置 ─────────────────────────────────────────── */

export interface SslOptions {
  key: string | null
  cert: string | null
  ca: string | null
  servername: string | null
  insecure: boolean | null
}

export interface SshOptions {
  host: string
  port: number
  username: string
  password?: string | null
  privatekey?: string | null
  privatekeybookmark?: string | null
  passphrase?: string | null
  timeout?: number | null
}

export interface SentinelOptions {
  masterName: string
  nodePassword?: string | null
}

/** redis_open_connection 的入参。存储态的连接请用 redis_open_stored_connection。 */
export interface ConnectionConfig {
  host: string
  port: number
  auth?: string | null
  username?: string | null
  db?: number | null
  name?: string | null
  separator?: string | null
  connectionReadOnly?: boolean | null
  cluster?: boolean | null
  sslOptions?: SslOptions | null
  sshOptions?: SshOptions | null
  sentinelOptions?: SentinelOptions | null
}

/** 列表项。凡是 has* 字段都表示"密钥链里存了这个秘密"，秘密本身永不出后端。 */
export interface ConnectionSummary {
  key: string
  name: string
  host: string
  port: number
  username: string | null
  sshHost: string | null
  sshPort: number | null
  sshUsername: string | null
  sentinelMasterName: string | null
  order: number | null
  color: string | null
  cluster: boolean
  readonly: boolean
  sshEnabled: boolean
  tlsEnabled: boolean
  sentinelEnabled: boolean
  hasAuth: boolean
  hasSshPassword: boolean
  hasSshPassphrase: boolean
  hasSentinelNodePassword: boolean
  hasTlsKey: boolean
  hasTlsCert: boolean
  hasTlsCa: boolean
}

export interface ConnectionStoreSshDraft {
  host: string
  port: number
  username: string
  password?: string | null
  clearPassword?: boolean | null
  privatekey?: string | null
  privatekeybookmark?: string | null
  passphrase?: string | null
  clearPassphrase?: boolean | null
  timeout?: number | null
}

export interface ConnectionStoreSentinelDraft {
  masterName: string
  nodePassword?: string | null
  clearNodePassword?: boolean | null
}

/** connection_store_get 的返回：密码字段一律为 null，只有 has* 标志。 */
export interface ConnectionEditDraft {
  key: string | null
  name: string | null
  host: string
  port: number
  username: string | null
  db: number | null
  separator: string | null
  connectionReadOnly: boolean | null
  cluster: boolean | null
  color: string | null
  order: number | null
  auth: string | null
  sslOptions: SslOptions | null
  sshOptions: ConnectionStoreSshDraft | null
  sentinelOptions: ConnectionStoreSentinelDraft | null
  hasAuth: boolean
  hasSshPassword: boolean
  hasSshPassphrase: boolean
  hasSentinelNodePassword: boolean
}

/**
 * connection_store_save 的入参。
 * 秘密字段的三态语义：undefined/null = 保持不变，字符串 = 覆写，
 * clearAuth/clearPassword/... = true 表示从密钥链里删除。
 */
export interface ConnectionSaveDraft {
  key?: string | null
  name?: string | null
  host: string
  port: number
  username?: string | null
  db?: number | null
  separator?: string | null
  connectionReadOnly?: boolean | null
  cluster?: boolean | null
  color?: string | null
  order?: number | null
  auth?: string | null
  clearAuth?: boolean | null
  sslOptions?: SslOptions | null
  sshOptions?: ConnectionStoreSshDraft | null
  sentinelOptions?: ConnectionStoreSentinelDraft | null
}

export interface ConnectionMigrationResult {
  migrated: string[]
  migratedIndexes: number[]
  skipped: string[]
  failed: string[]
}

export interface ConnectionOpened {
  connectionId: string
  connectionName: string
  /**
   * 这个会话的只读状态，与后端 `writable_runtime` 判的是同一个值。
   * 只读性属于**会话**而不是连接配置：会话在建立时就把它定死了，
   * 之后改配置也不会作用到它身上。所以前端只认这个字段，
   * 不要再从 ConnectionSummary / 编辑草稿自己推断一遍。
   */
  readonly: boolean
  /** 这是什么实现、什么版本。展示时写实现名 + `version`（「Valkey 8.1.1」），不要写 compatVersion。 */
  server: ServerIdentity
  /**
   * 建连时的能力判定，会话内可能被降级（`redis://capability-change`）。
   * 见 doc/backend/capabilities.md。
   */
  capabilities: Capabilities
}

/* ── 服务端身份与能力（Rust `services/capabilities.rs` 的镜像）──────── */

export type ServerFlavor = 'redis' | 'valkey' | 'dragonfly' | 'keydb' | 'unknown'

export interface ServerIdentity {
  flavor: ServerFlavor
  /** 实现自己的版本号（valkey_version / dragonfly_version / redis_version）。 */
  version: string | null
  /** `redis_version` 原值，只用于展示与排障：兼容实现在这里报的是兼容声明，不是能力集。 */
  compatVersion: string | null
}

/** 注册表的键。Rust 侧是 `CapabilityId` 枚举，两边手改。 */
export const CAPABILITY_IDS = {
  expireConditions: 'expireConditions',
  setKeepTtl: 'setKeepTtl',
  scripting: 'scripting',
} as const
export type CapabilityId = (typeof CAPABILITY_IDS)[keyof typeof CAPABILITY_IDS]

export type CapabilityState = 'supported' | 'unsupported' | 'unknown'

export interface Capability {
  state: CapabilityState
  /**
   * 按登记项的「未知时」策略折算后的结论。**界面只认它**：unknown 算能用还是不能用，
   * 每一项不同，判断依据只有后端的注册表知道。
   */
  usable: boolean
  /** 后端生成，界面原样显示。前端不要自己拼「该服务端是 Redis x」。 */
  reason: string
}

export type Capabilities = Record<CapabilityId, Capability>

export interface CapabilityChangeEvent {
  connectionId: string
  id: CapabilityId
  capability: Capability
}

/* ── 命令与值 ─────────────────────────────────────────── */

/**
 * RedisValue 是 `#[serde(untagged)]`，所以线上形态就是裸 JSON：
 *   Null → null / String → string / Integer → number
 *   Array → RedisValue[] / Object → { type: ..., ... }
 *
 * Object 分支带 type 判别字段，二进制、double、map 等都走这里。
 * 注意 Integer 是裸 number 而 Double 是 { type: 'double' }：
 * 判断数值时两条路都要走到。
 */
export type RedisValue =
  | null
  | string
  | number
  | RedisValue[]
  | RedisValueObject

export type RedisValueObject =
  | { type: 'binary'; base64: string }
  | { type: 'map'; entries: Array<{ key: RedisValue; value: RedisValue }> }
  | { type: 'attribute'; data: RedisValue; attributes: Array<{ key: RedisValue; value: RedisValue }> }
  | { type: 'double'; value: number | string }
  | { type: 'boolean'; value: boolean }
  | { type: 'verbatimString'; format: string; value: string }
  | { type: 'bigNumber'; value: string }
  | { type: 'push'; kind: string; data: RedisValue[] }
  | { type: 'serverError'; value: string }
  | { type: 'unsupported'; value: string }

/**
 * 值的统一形状，语义化取值命令一律用它。
 *
 * `text` 与 `base64` 恰好有一个非空：字节是合法 UTF-8 时只给 `text`
 * （UTF-8 可无损还原字节，不必每个值多传一份 base64），否则只给 `base64`。
 * 消费方一律 `text ?? base64`，只在展示层分支，不在结构上分支 ——
 * 这正是 `RedisValue` 的 String/Object 双形状要避免的。
 * 见 doc/backend/ipc-contract.md「RedisText」。
 */
export interface RedisText {
  text: string | null
  base64: string | null
  /**
   * 被截断时的原始总字节数，未截断为 null。
   * 非 null 时界面必须写出"显示前 N / 共 M"，且**编辑器必须只读** ——
   * 保存一个截断过的值会毁掉用户的数据。
   */
  totalBytes: number | null
}

export type RedisCommandArg =
  | { type: 'string'; value: string }
  | { type: 'binary'; base64: string }

export interface RedisCommandRequest {
  command: string
  args: RedisCommandArg[]
}

export interface CommandResult {
  command: string
  value: RedisValue
  elapsedMs: number
}

/* ── Key 扫描与详情 ───────────────────────────────────── */

export interface KeyEntry {
  key: string
  /** SCAN 不带类型，由后端在同一次往返里 pipeline 一批 TYPE 补上。 */
  keyType: string | null
  /** 只在 scanKeys 传了 withTtl 时才有；-1 = 永不过期。 */
  ttl: number | null
}

export interface KeyListResult {
  keys: KeyEntry[]
  cursor: string
  /** SCAN 游标回到 0。计数在此之前一律显示为 `312+`。 */
  completed: boolean
}

/**
 * Key 元信息头需要的全部字段，一次 IPC 拿回。
 *
 * size / encoding / memory / slot 都可能为 null —— 命令可能被 ACL 或
 * rename-command 拒掉、槽位只有集群才有。后端用 Option 表达"可能没有"，
 * 前端照实显示为空即可，不用再逐条 try/catch。
 */
export interface KeyMeta {
  key: string
  /** null = key 不存在（TYPE 返回 none） */
  keyType: string | null
  /** -1 = 永不过期，-2 = key 不存在，其余为剩余秒数 */
  ttl: number
  /** STRING 是字节数，其余类型是成员数 */
  size: number | null
  encoding: string | null
  memory: number | null
  slot: number | null
  /**
   * HyperLogLog 是 string 的**子形态**而不是第七种类型：TYPE 就返回 string，
   * keyType 不动、树与标签的圆点沿用 string 色。非空时 KeyTab 换用 HllViewer。
   * 识别只在打开 key 时做（后端 GETRANGE 0 15 校验 HYLL 头），SCAN 不做。
   */
  hll: HllMeta | null
}

export interface HllMeta {
  /** 头部第 5 字节：dense 定长 12,304 B，sparse 变长 */
  encoding: 'dense' | 'sparse'
}

/* ── HyperLogLog 结构查看器 ───────────────────────────
   镜像 services/hll.rs。所有字段按"能解析到哪一层"降级：
   伪魔数 → header 为 null；sparse 损坏 → registersBase64 为 null；
   dense 截断 → 数组仍是 16,384 长（缺失补零），validRegisters 划界。 */

export interface HllHeaderInfo {
  encoding: 'dense' | 'sparse'
  /** 头部缓存的基数（已剥掉失效标记位） */
  cachedCard: number
  /** 最高 bit 为 1：缓存自上次写入后未重算，读它得不到可信基数 */
  cacheStale: boolean
}

export interface HllInspect {
  header: HllHeaderInfo | null
  /** 展开后的寄存器，一字节一个值（0–63），恒 16,384 长 */
  registersBase64: string | null
  /** 可信寄存器个数；dense 截断时小于 16,384，缺失段画成占位 */
  validRegisters: number
  totalRegisters: number
  /** 服务端 STRLEN 报告的实际字节数 */
  actualBytes: number
  /** dense 恒 12,304；sparse 变长为 null */
  expectedBytes: number | null
}

/* ── 按类型划分的取值 ─────────────────────────────────
   参数与返回形状按每种类型真实的分页语义设计，不做统一抽象：
     Hash / Set   游标（无序、可能重复、COUNT 是提示不是保证）
     List / ZSet  下标区间（有序、精确 —— SCAN 家族给不了顺序）
     Stream       ID 区间（开闭语义特殊）
   见 doc/backend/ipc-contract.md「参数形状按分页语义」。 */

export interface HashEntry {
  field: RedisText
  value: RedisText
}

/** completed 是游标回到 0，不是"取够了"。 */
export interface HashPage {
  entries: HashEntry[]
  cursor: string
  completed: boolean
}

export interface MemberPage {
  members: RedisText[]
  cursor: string
  completed: boolean
}

/** 下标分页靠 total 判断边界，不像游标分页靠 cursor 回零。 */
export interface ListPage {
  items: RedisText[]
  /** 本页第一条在整个列表中的下标，用来显示行号 */
  start: number
  total: number
}

export interface ZSetMember {
  member: RedisText
  /** 分数是字符串：Redis 的分数是 double，inf / -inf 合法而 JSON 装不下 */
  score: string
}

export interface ZSetPage {
  members: ZSetMember[]
  start: number
  total: number
}

export interface StreamEntry {
  id: string
  fields: HashEntry[]
}

export interface StreamPage {
  entries: StreamEntry[]
  total: number
}

export interface StreamGroup {
  name: string
  consumers: number
  pending: number
  lastDeliveredId: string
}

/**
 * 一个值在库里的定位符，`redis_get_full_value` 的入参。
 *
 * **这不是 redis_get_key_detail 的覆辙**：那个撑不住是因为它把三种不同的
 * 分页语义（游标 / 下标 / ID 区间）塞进一个签名，参数互斥、返回形状随类型变。
 * 这里是同一个语义（按定位符取单个完整值）的三种定位形态，返回恒定是 RedisText，
 * 是真正的 sum type。
 *
 * **没有 Set / ZSet 变体，而且补不出来**：集合成员既是值又是定位符，要精确
 * 定位它得先有完整成员，而你正是因为它被截断了才要重取。这是 Redis 的数据模型，
 * 不是遗漏 —— 界面上要如实说出这个原因，不要只是把按钮禁用了事。
 * 两条都见 doc/backend/ipc-contract.md「完整值重取」。
 */
export type ValueRef =
  | { kind: 'string'; key: string }
  /** field 可以是任意字节，所以跟写入参数一样走 base64 */
  | { kind: 'hashField'; key: string; fieldBase64: string }
  | { kind: 'listIndex'; key: string; index: number }

/** 三种模式对应 PERSIST / EXPIRE / EXPIREAT，见 doc/frontend/ttl.md「三个模式直接对应三条命令」。 */
export type TtlMode = 'persist' | 'relative' | 'absolute'

/**
 * EXPIRE / EXPIREAT 的条件 flag。**Redis 7.0+**。
 *
 * 单 key 编辑用不上，批量必须有：没有条件的批量 TTL 会把范围内已经设好的
 * TTL 全部覆盖 —— 给 `cache:*` 补 TTL，会把里面本来设了 7 天的 key 全改成 1 天。
 * 见 doc/frontend/batch.md「条件 flag」。
 */
export type TtlCondition = 'nx' | 'xx' | 'gt' | 'lt'

export interface DatabaseInfo {
  index: number
  /** INFO keyspace 里见到的 key 数；该库为空时不出现，此处为 null */
  keys: number | null
}

export interface ServerMemory {
  used: string | null
  /** maxmemory 为 0 表示不限制，这时是 null 而不是 "0B" */
  max: string | null
}

/* ── 任务 ─────────────────────────────────────────────── */

export type TaskStatus = 'started' | 'progress' | 'completed' | 'cancelled' | 'failed'

export interface TaskStarted {
  taskId: string
}

export interface TaskStatusSnapshot {
  taskId: string
  connectionId: string
  connectionName: string
  taskType: string
  status: TaskStatus
  processed: number
  succeeded: number
  failed: number
  total: number | null
  /**
   * 扫描类任务（search_keys / load_all_keys）的近似进度，0..1。
   * 后端由 SCAN 游标的反转二进制估算（doc/backend/tasks.md「扫描任务：只报命中数」），
   * 任务内单调不减；其余任务类型恒为 null，据此不显示百分比。
   * **显示时必须带「约」字**——用 formatApproxPercent，别自己拼。
   */
  estimatedProgress: number | null
  message: string | null
}

export type TaskEventItem =
  | { type: 'clone'; snapshot: CloneTaskSnapshot }
  | {
      type: 'key'
      key: string
      /**
       * SCAN 不带类型，扫描任务逐页 pipeline 一批 TYPE 补上 —— 与 KeyEntry
       * 同一契约（树的类型点依赖它，见 doc/backend/tasks.md「事件项带 keyType」）。
       * key 在 SCAN 与 TYPE 之间被删掉、或 TYPE 被 ACL 挡掉时为 null。
       */
      keyType: string | null
      /** 只在任务选项开了 withTtl 时才有；-1 = 永不过期。 */
      ttl: number | null
    }
  | { type: 'memory'; key: string; size: number }
  | {
      type: 'monitor'
      timestamp: string
      database: number | null
      source: string | null
      args: string[]
      raw: string
    }
  | { type: 'pubSubMessage'; channel: string; pattern: string | null; message: string }
  | { type: 'file'; path: string }

export interface TaskProgressEvent {
  taskId: string
  connectionId: string
  connectionName: string
  taskType: string
  status: TaskStatus
  processed: number
  succeeded: number
  failed: number
  total: number | null
  /** 同 TaskStatusSnapshot.estimatedProgress：0..1、单调不减、显示必须带「约」。 */
  estimatedProgress: number | null
  items: TaskEventItem[]
  message: string | null
}

export interface KeyScanTaskOptions {
  pattern?: string | null
  count?: number | null
  /**
   * 与 scanKeys 的 withTtl 同义：每 key 再补一条 TTL，成本翻倍，
   * 由「外观 → Key 树 → 显示 TTL 标记」决定。缺省不取。
   */
  withTtl?: boolean | null
}

export interface BatchDeleteTaskOptions {
  keys: RedisCommandArg[]
  patterns: string[]
  /**
   * 只扫不删，用于试算。走**完全相同**的扫描与分块路径，只是不发 DEL。
   * 模式的命中数在 SCAN 之前谁也不知道，而删除不可撤销 ——
   * 试算的全部价值就在于「试算出来的数字就是真会删的数字」。
   */
  dryRun?: boolean | null
  count?: number | null
}

/**
 * 批量设置 TTL。`keys` 与 `patterns` 对应界面上的两种作用范围
 * （勾选的显式列表 / 匹配模式），与 BatchDeleteTaskOptions 同构。
 */
export interface BatchTtlTaskOptions {
  keys: RedisCommandArg[]
  patterns: string[]
  mode: TtlMode
  /** relative 是秒数、absolute 是 unix 时间戳；persist 时为 null */
  value?: number | null
  condition?: TtlCondition | null
  /**
   * 只扫不改，用于试算。走**完全相同**的扫描与分块路径，只是不发写命令。
   * 试算的全部价值就在于「试算出来的数字就是真会改的数字」。
   */
  dryRun?: boolean | null
  count?: number | null
}

export interface MemoryAnalysisTaskOptions {
  pattern?: string | null
  count?: number | null
  minSize?: number | null
  maxResults?: number | null
}

export type SubscriptionKind = 'subscribe' | 'psubscribe'

export interface SubscriptionTaskOptions {
  kind: SubscriptionKind
  channels: string[]
  patterns: string[]
}

export interface ExportKeysTaskOptions {
  keys: RedisCommandArg[]
  outputPath: string
}

/* ── 连接状态事件 ─────────────────────────────────────── */

export type ConnectionStatus = 'connecting' | 'ready' | 'failed' | 'closed'

export interface ConnectionStatusEvent {
  connectionId: string | null
  connectionKey: string | null
  connectionName: string
  status: ConnectionStatus
  code: string | null
  message: string | null
}

export interface CommandLogEvent {
  connectionId: string
  connectionName: string
  command: string
  args: string
  elapsedMs: number
  status: string
}

/* ── 原生能力 ─────────────────────────────────────────── */

export interface WindowState {
  isMaximized: boolean
  isMinimized: boolean
  isVisible: boolean
  scaleFactor: number
}

export interface UpdateCheckOptions {
  endpoint?: string | null
  currentVersion?: string | null
}

export type UpdateCheckStatus = 'available' | 'notAvailable' | 'failed' | 'notConfigured'

export interface UpdateCheckResult {
  status: UpdateCheckStatus
  currentVersion: string
  latestVersion: string | null
  releaseUrl: string | null
  message: string | null
}

export interface ProtoFile {
  path: string
  name: string
  content: string
}

/* ── 编解码器 ─────────────────────────────────────────── */

export const CODEC_NAMES = [
  'gzip',
  'deflate',
  'deflateRaw',
  'brotli',
  'msgpack',
  'pickle',
  'javaSerialize',
  'phpSerialize',
  'protobuf',
] as const
export type CodecName = (typeof CODEC_NAMES)[number]

/**
 * 三档能力，对应 codec_support_matrix()：
 *   rustRoundTrip     — Rust 侧可解可编，四种压缩算法
 *   rendererRoundTrip — 需要渲染进程参与，可解可编
 *   readonly          — 只能解，不能编回去（pickle / Java 序列化）
 */
export type CodecConclusion =
  | 'rustRoundTrip'
  | 'rendererRoundTrip'
  | 'readonly'
  | 'deferredUnsupported'

export interface CodecDecodeOptions {
  codec: CodecName
  inputBase64: string
}

/** 输入是 base64 字节而不是文本，与 decode 对称 —— msgpack 编完再 gzip 时，
 *  中间那一步的产物是二进制。 */
export interface CodecEncodeOptions {
  codec: CodecName
  inputBase64: string
}

export interface CodecDetectOptions {
  inputBase64: string
  candidates: CodecName[]
}

/**
 * 解码产物是字节不是文本：gzip 里可能裹着 msgpack，转成文本链式解码就走不通。
 * 见 doc/backend/codec.md「归属切分」。
 */
export interface CodecDecodeResult {
  value: RedisText
}

export interface CodecEncodeResult {
  outputBase64: string
}

export interface CodecDetectResult {
  codec: CodecName | null
}

/* ── 外部格式化器 ─────────────────────────────────────── */

export interface FormatterRunOptions {
  command: string
  params?: string | null
  valueBase64: string
  key?: string | null
  field?: string | null
  score?: string | null
  member?: string | null
  timeoutMs?: number | null
  maxOutputBytes?: number | null
  workingDir?: string | null
  /** Windows 上 .bat / .cmd 会被后端直接拒绝，与本开关无关。 */
  allowShell?: boolean | null
}

export interface FormatterRunResult {
  stdout: string
  stderr: string
  statusCode: number | null
  timedOut: boolean
  truncated: boolean
  previewCommand: string
}
/** 初始内容有类型语义；所有字符串值与成员传原始字节的 base64。 */
export type CreateKeyValue =
  | { type: 'string'; valueBase64: string }
  | { type: 'hash'; fields: { fieldBase64: string; valueBase64: string }[] }
  | { type: 'list'; elementsBase64: string[] }
  | { type: 'set'; membersBase64: string[] }
  | { type: 'zset'; members: { valueBase64: string; score: string }[] }
  | { type: 'stream'; fields: { fieldBase64: string; valueBase64: string }[] }

export type KeyExpiry =
  | { mode: 'persist' | 'preserve' }
  | { mode: 'relative' | 'absolute'; milliseconds: number }

export interface KeyWriteResult {
  keyType: string
}
export type ClonePhase = 'queued' | 'connecting' | 'reading' | 'restoring' | 'completed' | 'cancelled' | 'failed' | 'uncertain'
export interface CloneTaskSnapshot {
  taskId: string
  sourceConnectionId: string
  targetConnectionId: string
  sourceName: string
  targetName: string
  sourceKey: string
  targetKey: string
  phase: ClonePhase
  bytes: number | null
  keyType: string | null
  errorCode: string | null
  message: string
}
