import { call } from './invoke'
import type {
  CloneTaskSnapshot,
  CreateKeyValue,
  KeyExpiry,
  KeyWriteResult,
  BatchDeleteTaskOptions,
  BatchTtlTaskOptions,
  CommandResult,
  ConnectionConfig,
  ConnectionOpened,
  DatabaseInfo,
  ExportKeysTaskOptions,
  HashPage,
  HllInspect,
  KeyListResult,
  KeyMeta,
  KeyScanTaskOptions,
  ListPage,
  MemberPage,
  MemoryAnalysisTaskOptions,
  RedisText,
  ServerMemory,
  StreamGroup,
  StreamPage,
  SubscriptionTaskOptions,
  TaskStarted,
  TaskStatusSnapshot,
  TtlMode,
  ValueRef,
  ZSetPage,
} from '@/types/ipc'

/* ── 会话 ─────────────────────────────────────────────── */

export function openConnection(config: ConnectionConfig): Promise<ConnectionOpened> {
  return call('redis_open_connection', { config })
}

export function openStoredConnection(connectionKey: string, db?: number): Promise<ConnectionOpened> {
  return call('redis_open_stored_connection', { connectionKey, db: db ?? null })
}

export function closeConnection(connectionId: string): Promise<void> {
  return call('redis_close_connection', { connectionId })
}

/* ── 命令 ─────────────────────────────────────────────── */

/** CLI 标签页专用：整行文本由后端解析，引号规则见 services/cli.rs。 */
export function executeCli(connectionId: string, commandLine: string): Promise<CommandResult> {
  return call('redis_execute_cli', { connectionId, commandLine })
}

/*
 * 这里刻意**没有**通用的"发一条 Redis 命令"入口。
 *
 * v0.1 有 runCommand / executeCommand，于是整个应用的功能路径都从那里走，
 * 后端退化成透传管道：协议差异、分页语义、并发调度全泄漏进了 Vue 组件，
 * 只读守卫也因为同时要管 CLI 和功能路径而站错了位置。
 * 功能需求一律走上面按类型划分的语义化命令；CLI 走 executeCli。
 * 见 doc/backend/ipc-contract.md「不做」。
 */

/* ── Key ──────────────────────────────────────────────── */

export function createKey(connectionId: string, key: string, value: CreateKeyValue, expiry: KeyExpiry): Promise<KeyWriteResult> {
  return call('redis_create_key', { connectionId, key, value, expiry })
}

export function keyExists(connectionId: string, key: string): Promise<boolean> {
  return call('redis_key_exists', { connectionId, key })
}

export function startCloneKey(sourceConnectionId: string, targetConnectionId: string, sourceKey: string, targetKey: string, expiry: KeyExpiry): Promise<TaskStarted> {
  return call('redis_start_clone_key', { sourceConnectionId, targetConnectionId, sourceKey, targetKey, expiry })
}

export function getCloneTask(taskId: string): Promise<CloneTaskSnapshot> {
  return call('redis_get_clone_task', { taskId })
}

/**
 * 返回的每个 key 都带类型 —— 后端在同一次交互里 pipeline 了一批 TYPE。
 * `withTtl` 会再补一批 TTL，每 key 多一条命令，所以默认关闭，
 * 由「外观 → Key 树 → 显示 TTL 标记」决定。
 */
export function scanKeys(
  connectionId: string,
  cursor: string,
  pattern?: string | null,
  count?: number | null,
  withTtl = false,
): Promise<KeyListResult> {
  return call('redis_scan_keys', {
    connectionId,
    cursor,
    pattern: pattern ?? null,
    count: count ?? null,
    withTtl,
  })
}

/** Key 元信息头的全部字段，后端两批 pipeline 一次返回。 */
export function getKeyMeta(connectionId: string, key: string): Promise<KeyMeta> {
  return call('redis_get_key_meta', { connectionId, key })
}

/* ── 按类型取值 ───────────────────────────────────────
   每个类型一组命令，参数就是它真实的分页语义。
   RESP2/RESP3 的形状差异、集群路由都由后端吸收，这里不用管。 */

export function getStringValue(connectionId: string, key: string): Promise<RedisText> {
  return call('redis_get_string_value', { connectionId, key })
}

export function getHashEntries(
  connectionId: string,
  key: string,
  cursor: string,
  pattern?: string | null,
  count?: number | null,
): Promise<HashPage> {
  return call('redis_get_hash_entries', {
    connectionId,
    key,
    cursor,
    pattern: pattern ?? null,
    count: count ?? null,
  })
}

export function getSetMembers(
  connectionId: string,
  key: string,
  cursor: string,
  pattern?: string | null,
  count?: number | null,
): Promise<MemberPage> {
  return call('redis_get_set_members', {
    connectionId,
    key,
    cursor,
    pattern: pattern ?? null,
    count: count ?? null,
  })
}

/** 按下标分页 —— List 的插入顺序 SCAN 家族给不了。 */
export function getListRange(
  connectionId: string,
  key: string,
  start: number,
  stop: number,
): Promise<ListPage> {
  return call('redis_get_list_range', { connectionId, key, start, stop })
}

/** 按下标分页 —— ZSet 的分数序同理。 */
export function getZSetRange(
  connectionId: string,
  key: string,
  start: number,
  stop: number,
  rev = false,
): Promise<ZSetPage> {
  return call('redis_get_zset_range', { connectionId, key, start, stop, rev })
}

export function getStreamEntries(
  connectionId: string,
  key: string,
  start: string,
  end: string,
  count?: number | null,
  rev = false,
): Promise<StreamPage> {
  return call('redis_get_stream_entries', {
    connectionId,
    key,
    start,
    end,
    count: count ?? null,
    rev,
  })
}

export function getStreamGroups(connectionId: string, key: string): Promise<StreamGroup[]> {
  return call('redis_get_stream_groups', { connectionId, key })
}

/* ── HyperLogLog（string 的子形态）────────────────────
   识别在 getKeyMeta 里（meta.hll 非空），这里只有取数与写入。
   原始字节视图复用 getStringValue，不另设入口。 */

/**
 * 估算基数（PFCOUNT）。对非 HLL 值服务端会报 WRONGTYPE ——
 * HllViewer 靠这个错误进入"只读原始字节"的回退态，不要在这层吞掉。
 */
export function getHllCount(connectionId: string, key: string): Promise<number> {
  return call('redis_get_hll_count', { connectionId, key })
}

/** 结构查看器的数据：头部字段 + 展开成 16,384 个值的寄存器数组。 */
export function inspectHll(connectionId: string, key: string): Promise<HllInspect> {
  return call('redis_inspect_hll', { connectionId, key })
}

/** PFADD。返回"基数是否变化"。元素是任意字节，一律 base64。 */
export function addHllElements(
  connectionId: string,
  key: string,
  elementsBase64: string[],
): Promise<boolean> {
  return call('redis_add_hll_elements', { connectionId, key, elementsBase64 })
}

/**
 * 按定位符重取一个完整的值，上限 8 MB。
 *
 * 分页里的 Hash 字段值与集合成员是按 64 KiB 截断的，而半截 gzip 跟半截
 * msgpack 一样解不开 —— 想解码就得先有完整字节。
 *
 * **只能由用户显式触发**（点抽屉横幅上的「加载完整值」），不是把分页的上限
 * 放大：一页 200 条各 8 MB 是 1.6 GB。见 doc/backend/ipc-contract.md「完整值重取」。
 */
export function getFullValue(connectionId: string, target: ValueRef): Promise<RedisText> {
  return call('redis_get_full_value', { connectionId, target })
}

/* ── 写入 ─────────────────────────────────────────────
   值一律以 base64 字节传参：Redis 的值是任意字节，JSON 字符串装不下
   非 UTF-8 序列。用文本传参会让"把一个 gzip 值原样写回去"从一开始就不可能。
   只读拦截在后端入口按会话状态判断，不再靠命令名匹配。 */

export function setStringValue(
  connectionId: string,
  key: string,
  valueBase64: string,
): Promise<void> {
  return call('redis_set_string_value', { connectionId, key, valueBase64 })
}

export function setHashField(
  connectionId: string,
  key: string,
  fieldBase64: string,
  valueBase64: string,
): Promise<void> {
  return call('redis_set_hash_field', { connectionId, key, fieldBase64, valueBase64 })
}

export function deleteHashFields(
  connectionId: string,
  key: string,
  fieldsBase64: string[],
): Promise<number> {
  return call('redis_delete_hash_fields', { connectionId, key, fieldsBase64 })
}

export function addSetMembers(
  connectionId: string,
  key: string,
  membersBase64: string[],
): Promise<number> {
  return call('redis_add_set_members', { connectionId, key, membersBase64 })
}

export function deleteSetMembers(
  connectionId: string,
  key: string,
  membersBase64: string[],
): Promise<number> {
  return call('redis_delete_set_members', { connectionId, key, membersBase64 })
}

export function setListItem(
  connectionId: string,
  key: string,
  index: number,
  valueBase64: string,
): Promise<void> {
  return call('redis_set_list_item', { connectionId, key, index, valueBase64 })
}

export function pushListItem(
  connectionId: string,
  key: string,
  valueBase64: string,
  tail: boolean,
): Promise<number> {
  return call('redis_push_list_item', { connectionId, key, valueBase64, tail })
}

/**
 * 按下标删除一个 List 元素。
 *
 * Redis 没有这个命令，后端用 Lua 把 `LSET` 哨兵 + `LREM` 包成原子操作。
 * v0.1 是前端分两条发的 —— 中间任何一次并发写都会让 LREM 删到别的元素上，
 * 而且用户永远不会知道。
 */
export function deleteListItem(connectionId: string, key: string, index: number): Promise<boolean> {
  return call('redis_delete_list_item', { connectionId, key, index })
}

export function addZSetMember(
  connectionId: string,
  key: string,
  memberBase64: string,
  score: string,
): Promise<void> {
  return call('redis_add_zset_member', { connectionId, key, memberBase64, score })
}

export function deleteZSetMembers(
  connectionId: string,
  key: string,
  membersBase64: string[],
): Promise<number> {
  return call('redis_delete_zset_members', { connectionId, key, membersBase64 })
}

/** 三种模式对应 PERSIST / EXPIRE / EXPIREAT，见 doc/frontend/ttl.md「三个模式直接对应三条命令」。 */
export function setKeyTtl(
  connectionId: string,
  key: string,
  mode: TtlMode,
  value?: number | null,
): Promise<void> {
  return call('redis_set_key_ttl', { connectionId, key, mode, value: value ?? null })
}

export function renameKey(connectionId: string, key: string, newKey: string): Promise<void> {
  return call('redis_rename_key', { connectionId, key, newKey })
}

export function deleteKeys(connectionId: string, keys: string[]): Promise<number> {
  return call('redis_delete_keys', { connectionId, keys })
}

/* ── 会话级信息 ───────────────────────────────────────── */

/**
 * DB 列表。CONFIG GET databases 拿不到时后端自己退回 INFO keyspace 推断 ——
 * v0.1 这条回退链在前端，它需要知道"CONFIG 可能被只读守卫拒掉"这件后端的事。
 */
export function getDatabaseList(connectionId: string): Promise<DatabaseInfo[]> {
  return call('redis_get_database_list', { connectionId })
}

export function getServerMemory(connectionId: string): Promise<ServerMemory> {
  return call('redis_get_server_memory', { connectionId })
}

/* ── 任务 ─────────────────────────────────────────────── */

export function cancelTask(taskId: string): Promise<TaskStatusSnapshot> {
  return call('redis_cancel_task', { taskId })
}

export function getTaskStatus(taskId: string): Promise<TaskStatusSnapshot> {
  return call('redis_get_task_status', { taskId })
}

/**
 * 暂停 / 恢复一个任务（窗口隐藏时暂停、显示时恢复；目前只有扫描任务在页间检查）。
 * 暂停是运行时标志而不是新的任务状态 —— 返回的快照 status 不会因此改变，
 * 恢复后任务从当前游标继续。见 doc/backend/tasks.md「扫到底的扫描任务」。
 */
export function setTaskPaused(taskId: string, paused: boolean): Promise<TaskStatusSnapshot> {
  return call('redis_set_task_paused', { taskId, paused })
}

/**
 * 扫到底的任务：key 经 redis://task-progress 流式回传，事件项自带 keyType
 * （withTtl 开着时 TTL 同批），与 scanKeys 的 KeyEntry 同一契约。
 */
export function startLoadAllKeys(
  connectionId: string,
  options: KeyScanTaskOptions,
): Promise<TaskStarted> {
  return call('redis_start_load_all_keys', { connectionId, options })
}

/** 与 startLoadAllKeys 同一条管线，只是任务类型不同（search_keys）。 */
export function startSearchKeys(
  connectionId: string,
  options: KeyScanTaskOptions,
): Promise<TaskStarted> {
  return call('redis_start_search_keys', { connectionId, options })
}

export function startBatchDelete(
  connectionId: string,
  options: BatchDeleteTaskOptions,
): Promise<TaskStarted> {
  return call('redis_start_batch_delete', { connectionId, options })
}

/**
 * 批量设置 TTL。
 *
 * 它是 task 而不是语义化命令：长耗时、可取消、带进度。
 * 语义化命令里刻意没有它 —— 那一层是「一个用户动作 = 一个命令」的映射，
 * 往里面塞一个「批量」，这条线就松了。
 * 见 doc/backend/ipc-contract.md「一个用户动作一个命令」。
 */
export function startBatchTtl(
  connectionId: string,
  options: BatchTtlTaskOptions,
): Promise<TaskStarted> {
  return call('redis_start_batch_ttl', { connectionId, options })
}

/**
 * 内存分析走任务管线：它要扫遍模式命中的整个 keyspace，长耗时、可取消、带进度。
 * 只覆盖已加载行的逐 key 读数在 Key 浏览器的分析模式里（`redis_get_key_meta`）。
 */
export function startMemoryAnalysis(
  connectionId: string,
  options: MemoryAnalysisTaskOptions,
): Promise<TaskStarted> {
  return call('redis_start_memory_analysis', { connectionId, options })
}

export function startMonitor(connectionId: string): Promise<TaskStarted> {
  return call('redis_start_monitor', { connectionId })
}

export function startSubscription(
  connectionId: string,
  options: SubscriptionTaskOptions,
): Promise<TaskStarted> {
  return call('redis_start_subscription', { connectionId, options })
}

export function startImportKeys(connectionId: string, filePath: string): Promise<TaskStarted> {
  return call('redis_start_import_keys', { connectionId, filePath })
}

export function startImportCommands(connectionId: string, filePath: string): Promise<TaskStarted> {
  return call('redis_start_import_commands', { connectionId, filePath })
}

export function startExportKeys(
  connectionId: string,
  options: ExportKeysTaskOptions,
): Promise<TaskStarted> {
  return call('redis_start_export_keys', { connectionId, options })
}
