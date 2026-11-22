import { defineStore } from 'pinia'
import { computed, reactive, ref, shallowRef, triggerRef, watch } from 'vue'
import * as redis from '@/api/redis'
import { getConnection } from '@/api/connections'
import { AppFault, toFault } from '@/api/invoke'
import {
  buildKeyTree,
  isPrefixCovered,
  matchPatternFor,
  scopedSearchPattern,
  type KeyNode,
} from '@/lib/keyTree'
import type {
  Capabilities,
  CapabilityChangeEvent,
  ConnectionEditDraft,
  ConnectionStatus,
  KeyEntry,
  ServerIdentity,
} from '@/types/ipc'
import { useConnectionsStore } from './connections'
import { useSettingsStore } from './settings'
import { useTasksStore } from './tasks'

/**
 * 运行态会话。一个会话 = 一次 redis_open_connection，且它是 **DB 级** 的：
 * 后端在开连接时就把 db 定死了，SELECT 又在只读守卫的写命令之列，
 * 所以切 DB 的正确做法是再开一个会话，而不是在现有会话上切。
 */
export interface Session {
  id: string
  connectionKey: string | null
  name: string
  db: number
  readonly: boolean
  /** 建连时探测的身份。一个会话期间它不会变。 */
  server: ServerIdentity
  /**
   * 建连时的能力判定。会话内只会被降级（`applyCapabilityChange`），不会变好 ——
   * 要重新探测就重开会话。功能置灰一律读 `usable`，提示原样用 `reason`。
   */
  capabilities: Capabilities
  cluster: boolean
  separator: string
  status: ConnectionStatus
  error: AppFault | null

  /* keyspace */
  keys: KeyEntry[]
  cursor: string
  scanComplete: boolean
  scanning: boolean
  /**
   * 扫描代际号，会话内部状态，不进 IPC 契约。
   *
   * `rescan` / `startSearch` 每次递增，`scanMore` 在发请求前捕获快照，响应
   * 回来时对不上就整体丢弃 —— 否则初扫在途时改搜索词，旧 pattern 的那批 key
   * 会推进刚清空的数组，游标也被拨到旧扫描的续点上。重扫不能等在途扫描回来
   * 再发（scanning 守卫会把新扫描静默弹掉），只能让旧响应作废。
   * 搜索走任务，task_id 就是代际令牌；浏览路径（重新扫描、清空搜索回浏览态、
   * 加载更多）走 scanMore，没有 task_id，靠的就是它。
   */
  scanGeneration: number
  pattern: string
  /** 已加载 key 数触到 treeNodeLimit：底部行改说「已达上限」，仍可手动继续加载 */
  truncated: boolean

  /* 搜索态（doc/frontend/key-tree.md「搜索态」）。'idle' = 浏览态；其余状态下树里装的是
     search_keys 任务流式并入的结果，「加载更多」行让位给任务状态行。 */
  searchStatus: SearchStatus
  /**
   * 搜索任务的近似进度（0..1，null = 不可用，doc/backend/tasks.md「扫描任务：只报命中数」）。
   * 后端由 SCAN 游标估算、已做单调不减，随任务进度事件到达。
   * 只在 running 态显示，且必须带「约」（formatApproxPercent）。
   */
  searchProgress: number | null
  /** 精确命中（doc/frontend/key-tree.md「精确命中」）：输入不含 glob 元字符时并行探测的结果，置顶显示。 */
  searchExact: SearchExactHit | null
  /**
   * 前缀作用域（「在此搜索」，doc/frontend/key-tree.md「文件夹内搜索与前缀权威」）：非空时搜索限定在该前缀下，
   * pattern = escapeGlob(前缀 + 分隔符) + 用户词。词为空即「扫描此前缀」
   * 的搜索态效果。会话内存态，切会话时随会话自己恢复。
   */
  searchScope: string | null
  /**
   * 搜索框原词（trim 后）。切会话恢复用它而不是拼好的 pattern ——
   * 作用域已经在 chip 里，把整个 pattern 回填进输入框会把前缀重复一遍。
   */
  searchQuery: string
}

/**
 * 'completed'（扫完）与 'cancelled' / 'failed'（中途停下）必须分开：
 * 前者的结果是权威的（计数摘 `+`、空结果 = 「没有匹配的 Key」），
 * 后两者的列表不完整，界面绝不能让它长得像完整的（doc/frontend/key-tree.md「诚实信号」）。
 */
export type SearchStatus = 'idle' | 'running' | 'completed' | 'limited' | 'cancelled' | 'failed'

export interface SearchExactHit {
  key: string
  keyType: string | null
}

/**
 * 每个会话的搜索任务挂载点。**刻意不进 reactive**：buffer 每页都在变，
 * 进了响应式代理，合帧就白做了 —— 界面只依赖 Session 上的 searchStatus /
 * searchExact 与 keys（flush 时才动）。
 *
 * epoch 是 task_id 这个代际令牌（doc/frontend/key-tree.md「task_id 就是代际令牌」）在本地的影子：任务还没拿到 id
 * 的窗口期（startSearchKeys 在途）也要能作废刚发起的那次搜索。
 */
interface SearchRuntime {
  epoch: number
  taskId: string | null
  unsubscribe: (() => void) | null
  cancelFlush: (() => void) | null
  buffer: KeyEntry[]
  /** 按 key 名去重：SCAN 在 rehash 期间会重复返回同一 key，精确命中也可能撞上 */
  seen: Set<string>
}

/**
 * 前缀扫描（「扫描此前缀」与前缀权威，doc/frontend/key-tree.md「文件夹内搜索与前缀权威」）的任务挂载点，同 SearchRuntime 的
 * 理由不进 reactive。它是**浏览态的叠加层**，不是搜索：树不进搜索态，
 * 结果按 key 名去重后并入 session.keys。
 *
 * seen 以任务启动时的 session.keys 为去重基线 —— 同一 key 可能已被全局
 * 扫描加载过。knownLength 用来侦测别的写入者（浏览续扫 / 删除 / rememberKey）
 * 动过 keys：动过就重建基线，旧基线既可能漏判也可能误判。
 */
interface PrefixScanRuntime {
  epoch: number
  taskId: string | null
  unsubscribe: (() => void) | null
  cancelFlush: (() => void) | null
  buffer: KeyEntry[]
  seen: Set<string>
  knownLength: number
}

/** 合帧调度：浏览器里按帧，测试环境（node）退回宏任务。返回取消函数。 */
function scheduleFrame(callback: () => void): () => void {
  if (typeof requestAnimationFrame === 'function') {
    const id = requestAnimationFrame(() => callback())
    return () => cancelAnimationFrame(id)
  }
  const id = setTimeout(callback, 16)
  return () => clearTimeout(id)
}

/** Redis glob 的元字符（stringmatchlen 认 `* ? [ ] \`）。命中则输入是模式，不做精确探测。 */
const GLOB_META = /[\\*?[\]]/

export interface DbInfo {
  index: number
  keys: number | null
}

function sessionKeyOf(connectionKey: string, db: number) {
  return `${connectionKey}/${db}`
}

export const useSessionsStore = defineStore('sessions', () => {
  const settings = useSettingsStore()
  const connections = useConnectionsStore()
  const tasks = useTasksStore()

  const sessions = reactive(new Map<string, Session>())
  /** sessionId → 搜索任务挂载点。普通 Map，见 SearchRuntime 的说明。 */
  const searchRuntimes = new Map<string, SearchRuntime>()
  /** sessionId → prefix → 前缀扫描挂载点。普通 Map，理由同上。 */
  const prefixRuntimes = new Map<string, Map<string, PrefixScanRuntime>>()
  /**
   * 「扫描类任务当前应当暂停」的期望态（窗口隐藏 = true，doc/frontend/key-tree.md「取消时机」）。
   * setSearchTasksPaused 的广播只够得着已有 taskId 的任务；启动 invoke
   * 在途的任务收不到，拿到 taskId 后要按这份期望态补发一次 ——
   * 否则窗口在启动间隙隐藏时，该任务会全速跑到底。
   */
  let scanTasksPaused = false
  /**
   * 前缀权威集（doc/frontend/key-tree.md「前缀权威」）：sessionId → 已扫到底的**完整前缀**集合（不是行索引 ——
   * 与折叠态同一个教训，见 doc/frontend/key-tree.md「增量到达会改变合并结果」）。会话内存态，不持久化。
   * 作废时机：rescan、进搜索态、keyspace 版本号变化、会话关闭；
   * 全局 scanComplete 后所有前缀天然权威，集合清空即可。
   * reactive：树上的计数要靠它摘 `+`。
   */
  const authoritativePrefixes = reactive(new Map<string, Set<string>>())
  /** 扫描进行中的前缀（计数位换 spinner 用）。与权威集分开存：
      任务取消/失败要把进行中清干净，但绝不能置权威。 */
  const scanningPrefixes = reactive(new Map<string, Set<string>>())
  /** connectionKey/db → sessionId，用来判断某个 DB 是否已经开着 */
  const openedDbs = reactive(new Map<string, string>())
  /** connectionKey → 该实例的 DB 列表 */
  const dbLists = reactive(new Map<string, DbInfo[]>())

  /**
   * 正在建连的 connectionKey。
   *
   * 建连不是瞬时的：跨 WAN 的 RTT、TLS 握手、SSH 隧道，几秒钟都算正常，
   * 凭据错误在后端还要重试一次。这段时间里侧栏必须有反应 —— v0.1 里
   * 点下去到报错之间界面是完全静止的，用户只能重复点击。
   */
  const connecting = reactive(new Set<string>())

  function isConnecting(connectionKey: string): boolean {
    return connecting.has(connectionKey)
  }

  /* 侧栏展开态。key 树的展开态按完整前缀存储，
     否则合并行裂开后折叠态会错位（doc/frontend/key-tree.md「增量到达会改变合并结果」）。 */
  const expandedConnections = reactive(new Set<string>())
  const expandedDbs = reactive(new Set<string>())
  const expandedPaths = reactive(new Map<string, Set<string>>())

  const selected = ref<{ sessionId: string; path: string } | null>(null)

  /**
   * 每个会话的 keyspace 版本号。
   *
   * 批量操作改的是一个**范围**，可能波及任何一个打开着的 key 标签页，
   * 让每个标签页自己去猜有没有被覆盖不现实。所以给会话一个版本号，
   * 批量任务完成后 +1，标签页 watch 它并重取读数。
   *
   * 只刷读数，**不动标签页状态** ——「写入即转正」的判据是"你对*它*动过手"，
   * 而批量是对一个范围动手，把碰巧开着的预览页转正会莫名其妙。
   * 见 doc/frontend/tabs.md「写入即转正」。
   *
   * 树不跟着刷：它本来就是"已加载的快照"，重扫是重操作，界面上已经有
   * 「重新扫描」这个显式入口。
   */
  const keyspaceRevision = reactive(new Map<string, number>())

  function revisionOf(sessionId: string): number {
    return keyspaceRevision.get(sessionId) ?? 0
  }

  function invalidateKeyspace(sessionId: string) {
    keyspaceRevision.set(sessionId, revisionOf(sessionId) + 1)
    // 批量操作 / 导入改动的是一个范围，「这个前缀已扫到底」的承诺随之失效
    authoritativePrefixes.get(sessionId)?.clear()
  }

  /** 建树开销大，用 shallowRef + 显式 trigger，避免深层响应式代理整棵树。 */
  const treeCache = shallowRef(new Map<string, KeyNode>())

  const activeSessionId = ref<string | null>(null)
  const activeSession = computed(() =>
    activeSessionId.value ? sessions.get(activeSessionId.value) ?? null : null,
  )

  /* separator 与默认 db 只存在于编辑草稿里，ConnectionSummary 不带。
     每次展开都去读一次文件没必要，缓存住；保存连接后由 invalidateDraft 清掉。 */
  const draftCache = new Map<string, ConnectionEditDraft>()

  async function draftOf(connectionKey: string): Promise<ConnectionEditDraft> {
    const cached = draftCache.get(connectionKey)
    if (cached) return cached
    const draft = await getConnection(connectionKey)
    draftCache.set(connectionKey, draft)
    return draft
  }

  function invalidateDraft(connectionKey: string) {
    draftCache.delete(connectionKey)
  }

  function pathsOf(sessionId: string): Set<string> {
    let set = expandedPaths.get(sessionId)
    if (!set) {
      set = new Set<string>()
      expandedPaths.set(sessionId, set)
    }
    return set
  }

  /**
   * 列表模式必须在**建树**时就用空分隔符，而不是只在摊平时不合并 ——
   * 后者只是不做紧凑合并，命名空间层级还在，出来的仍是一棵树。
   * 空分隔符让每个 key 直接挂到根下，这才是真正的扁平列表。
   */
  function separatorFor(session: Session): string {
    return settings.state.defaultTreeMode === 'list' ? '' : session.separator
  }

  function rebuildTree(session: Session) {
    treeCache.value.set(session.id, buildKeyTree(session.keys, separatorFor(session)))
    triggerRef(treeCache)
  }

  /* 切换树形/列表要重建所有已打开会话的树，否则只有下次扫描的那个会变 */
  watch(
    () => settings.state.defaultTreeMode,
    () => {
      for (const session of sessions.values()) rebuildTree(session)
    },
  )

  function treeOf(sessionId: string): KeyNode | null {
    return treeCache.value.get(sessionId) ?? null
  }

  /* ── 打开 / 关闭 ─────────────────────────────────────── */

  async function openDb(connectionKey: string, db: number): Promise<string> {
    const existing = openedDbs.get(sessionKeyOf(connectionKey, db))
    if (existing && sessions.has(existing)) {
      activeSessionId.value = existing
      return existing
    }

    connecting.add(connectionKey)
    try {
      return await openDbInner(connectionKey, db)
    } finally {
      // 失败路径也要清掉，否则那一行会一直转圈
      connecting.delete(connectionKey)
    }
  }

  async function openDbInner(connectionKey: string, db: number): Promise<string> {
    const draft = await draftOf(connectionKey)
    const summary = connections.byKey.get(connectionKey)
    const opened = await redis.openStoredConnection(connectionKey, db)

    const session: Session = {
      id: opened.connectionId,
      connectionKey,
      name: opened.connectionName,
      db,
      // 只读一律取后端会话的判定，不从配置推断：那样是两套判断，
      // 会话建立后配置改了就会漂移成"界面能编辑、守卫却拦"或者反过来
      readonly: opened.readonly,
      server: opened.server,
      capabilities: opened.capabilities,
      cluster: !!(draft.cluster ?? summary?.cluster),
      separator: draft.separator ?? ':',
      status: 'ready',
      error: null,
      keys: [],
      cursor: '0',
      scanComplete: false,
      scanning: false,
      scanGeneration: 0,
      pattern: '',
      truncated: false,
      searchStatus: 'idle',
      searchProgress: null,
      searchExact: null,
      searchScope: null,
      searchQuery: '',
    }

    sessions.set(session.id, session)
    openedDbs.set(sessionKeyOf(connectionKey, db), session.id)
    rebuildTree(session)
    activeSessionId.value = session.id
    connections.markUsed(connectionKey)

    await Promise.all([loadDbList(connectionKey, session.id), scanMore(session.id)])
    return session.id
  }

  /**
   * 展开一个实例：按它自己配置的默认 db 打开会话，而不是无脑 db0 ——
   * 配成 db3 的连接展开后应该直接落在 db3。
   */
  async function openConnection(connectionKey: string) {
    const draft = await draftOf(connectionKey)
    const db = draft.db ?? 0
    await openDb(connectionKey, db)
    expandedConnections.add(connectionKey)
    expandedDbs.add(sessionKeyOf(connectionKey, db))
  }

  async function closeSession(sessionId: string) {
    const session = sessions.get(sessionId)
    if (!session) return
    // 会话关闭也是搜索任务的取消时机之一（doc/frontend/key-tree.md「取消时机」）：先取消再断连，
    // 否则任务只能等下一页失败在「Connection is closed」上
    teardownSearchTask(searchRuntimeOf(sessionId))
    searchRuntimes.delete(sessionId)
    // 前缀扫描同理；权威态是会话内存态，会话重开即作废
    cancelPrefixScans(sessionId)
    prefixRuntimes.delete(sessionId)
    authoritativePrefixes.delete(sessionId)
    scanningPrefixes.delete(sessionId)
    try {
      await redis.closeConnection(sessionId)
    } catch {
      // 会话可能已经在后端消失，前端状态照常清理
    }
    if (session.connectionKey) {
      openedDbs.delete(sessionKeyOf(session.connectionKey, session.db))
    }
    sessions.delete(sessionId)
    expandedPaths.delete(sessionId)
    treeCache.value.delete(sessionId)
    triggerRef(treeCache)
    if (activeSessionId.value === sessionId) activeSessionId.value = null
    if (selected.value?.sessionId === sessionId) selected.value = null
  }

  /** 某个实例当前开着的所有会话 id。改配置后要连它们一起作废。 */
  function sessionIdsFor(connectionKey: string): string[] {
    return Array.from(sessions.values())
      .filter((s) => s.connectionKey === connectionKey)
      .map((s) => s.id)
  }

  /** 关闭一个实例下的所有 DB 会话。 */
  async function closeConnection(connectionKey: string) {
    for (const id of sessionIdsFor(connectionKey)) await closeSession(id)
    expandedConnections.delete(connectionKey)
    dbLists.delete(connectionKey)
  }

  /* ── DB 列表 ─────────────────────────────────────────── */

  /**
   * DB 列表整个交给后端：集群只有 db0，非集群下 `CONFIG GET databases` 拿总数，
   * 拿不到时退回 `INFO keyspace` 见到的最大下标再兜底 16。
   *
   * v0.1 这条回退链在前端 —— 它需要知道"CONFIG 可能被只读守卫拒掉"这件
   * 纯属后端的事。见 doc/backend/sessions.md「只读属于会话」。
   */
  async function loadDbList(connectionKey: string, sessionId: string) {
    if (!sessions.has(sessionId)) return
    try {
      dbLists.set(connectionKey, await redis.getDatabaseList(sessionId))
    } catch {
      // 连 INFO 都拿不到时至少显示当前 DB，别让侧栏空着
      const session = sessions.get(sessionId)
      dbLists.set(connectionKey, [{ index: session?.db ?? 0, keys: null }])
    }
  }

  /* ── 扫描 ─────────────────────────────────────────────── */

  async function scanMore(sessionId: string) {
    const session = sessions.get(sessionId)
    if (!session || session.scanning || session.scanComplete) return
    // 搜索态没有「加载更多」：树里是任务流式并入的结果，游标扫描一旦混进来
    // 就是两条管线往同一个数组里写。回浏览态的唯一入口是 rescan。
    if (session.searchStatus !== 'idle') return

    // await 期间 rescan 可能把会话翻到新代际，这个快照决定响应回来时还认不认
    const generation = session.scanGeneration
    session.scanning = true
    try {
      const result = await redis.scanKeys(
        sessionId,
        session.cursor,
        session.pattern || null,
        settings.state.scanCount,
        // 树上显示 TTL 意味着每 key 多一条 TTL，所以跟着设置项走
        settings.state.treeTtl,
      )
      // 代际不符 = 这批结果属于一次已作废的扫描：keys / cursor /
      // scanComplete / error 一律不写，半新半旧的状态比丢一批更糟
      if (session.scanGeneration !== generation) return
      // 并入按 key 名去重（doc/frontend/key-tree.md「前缀权威」）：前缀叠加层是与浏览续扫并行的第二条
      // 写入管线，它已扫到底并入过的 key 必然还会出现在游标续扫的后续批次
      // 里（flushPrefixItems 只挡「叠加层重复浏览已有的」这一个方向）；
      // SCAN 在 rehash 期间同一条游标链也会重复吐 key。每批临时建 Set 是
      // O(n+m)，n 有 treeNodeLimit 兜底；刻意不用常驻 Set —— 那要与
      // forgetKey / forgetPrefix / rescan / 搜索清空等所有写点保持同步，
      // 漏一处就是反向的幽灵重复，每批重建没有这种远距离耦合。
      const known = new Set(session.keys.map((entry) => entry.key))
      const fresh = result.keys.filter((entry) => {
        if (known.has(entry.key)) return false
        known.add(entry.key)
        return true
      })
      session.keys.push(...fresh)
      session.cursor = result.cursor
      session.scanComplete = result.completed
      // 全局扫完后所有前缀天然权威，权威集清空即可
      if (result.completed) authoritativePrefixes.get(sessionId)?.clear()
      session.truncated = session.keys.length >= settings.state.treeNodeLimit
      session.error = null
      rebuildTree(session)
    } catch (error) {
      // 旧代际的失败同样不算数 —— 它报告的是没人关心的那次扫描
      if (session.scanGeneration !== generation) return
      session.error = toFault(error)
    } finally {
      // scanning 归当前代际所有：rescan 已把标志让渡给新扫描，
      // 旧响应迟到时不能把正在进行的新扫描标成未进行
      if (session.scanGeneration === generation) session.scanning = false
    }
  }

  /** 改模式或手动刷新：清空已加载的 key，游标归零重扫。
      返回类型显式标注：与 startSearch 互相调用，推断在环上会成 any。 */
  async function rescan(sessionId: string, pattern?: string): Promise<void> {
    const session = sessions.get(sessionId)
    if (!session) return
    // 刷新动作（不传 pattern：刷新按钮 / F5 / 右键「重新扫描」）落在搜索态
    // （有词或有作用域）时是**重启搜索任务**，不是退回浏览的单批扫描 ——
    // 否则会造出「搜索 pattern + 加载更多（已加载 N）」的杂交态：搜索结果
    // 只扫了一批，却长得像可以手点续扫的浏览视图（doc/frontend/key-tree.md「搜索态」）。
    // 在 store 层分派，三个触发点行为一致。显式传 pattern 的路径不受影响：那是清空搜索回浏览态的语义。
    if (pattern === undefined && (session.searchQuery || session.searchScope !== null)) {
      return startSearch(sessionId, session.searchQuery)
    }
    // 回浏览态一律先取消搜索任务（清空搜索、重新扫描按钮、右键重新扫描
    // 都走这里）：不能留一个还在往刚清空的树里灌结果的任务
    teardownSearchTask(searchRuntimeOf(sessionId))
    // 基础视图即将清空重来：前缀扫描的叠加层与权威集一起作废（doc/frontend/key-tree.md「前缀权威」）
    cancelPrefixScans(sessionId)
    authoritativePrefixes.get(sessionId)?.clear()
    session.searchStatus = 'idle'
    session.searchProgress = null
    session.searchExact = null
    // 显式传 pattern（今天只有空串）= 重置搜索框状态机：作用域与词一起清掉。
    // 不传 = 单纯重扫当前视图（刷新按钮 / 右键重新扫描），chip 与词留在原地。
    if (pattern !== undefined) {
      session.searchScope = null
      session.searchQuery = ''
    }
    // 翻代际并释放 scanning：在途的旧扫描就地作废（响应到达时按代际丢弃），
    // 新扫描不等它回来 —— 否则 scanMore 的守卫会把这次重扫静默弹掉，
    // 树里留着旧 pattern 的数据，下次「加载更多」还会拿旧游标配新 pattern 续扫
    session.scanGeneration += 1
    session.scanning = false
    if (pattern !== undefined) session.pattern = pattern
    session.keys = []
    session.cursor = '0'
    session.scanComplete = false
    session.truncated = false
    rebuildTree(session)
    await scanMore(sessionId)
  }

  /* ── 搜索 = 扫到底的任务（doc/frontend/key-tree.md「搜索」）───────── */

  function searchRuntimeOf(sessionId: string): SearchRuntime {
    let runtime = searchRuntimes.get(sessionId)
    if (!runtime) {
      runtime = {
        epoch: 0,
        taskId: null,
        unsubscribe: null,
        cancelFlush: null,
        buffer: [],
        seen: new Set(),
      }
      searchRuntimes.set(sessionId, runtime)
    }
    return runtime
  }

  /**
   * 拆掉当前搜索任务的一切挂载：翻代际、退订、丢缓冲、通知后端取消。
   * 只管任务，不动会话的 searchStatus —— 那由各调用方按语义设置。
   *
   * 后端取消是义务而不是优化：只停止消费不取消，服务端会继续全速扫、
   * 喂给一个正在丢弃结果的客户端（doc/frontend/key-tree.md「取消时机」）。
   */
  function teardownSearchTask(runtime: SearchRuntime) {
    runtime.epoch += 1
    runtime.unsubscribe?.()
    runtime.unsubscribe = null
    runtime.cancelFlush?.()
    runtime.cancelFlush = null
    runtime.buffer = []
    if (runtime.taskId) {
      void redis.cancelTask(runtime.taskId).catch(() => undefined)
      runtime.taskId = null
    }
  }

  /**
   * 把缓冲里的 key 并入会话并重建树。合帧的落点：任务每页一个事件，
   * 逐事件 rebuildTree 是 O(n) × 页数，这里一帧只走一次。
   */
  function flushSearchItems(session: Session, runtime: SearchRuntime) {
    const limit = settings.state.treeNodeLimit
    if (runtime.buffer.length) {
      const room = Math.max(0, limit - session.keys.length)
      const merged = runtime.buffer.length > room ? runtime.buffer.slice(0, room) : runtime.buffer
      runtime.buffer = []
      if (merged.length) {
        session.keys.push(...merged)
        rebuildTree(session)
      }
    }
    if (session.searchStatus === 'running' && session.keys.length >= limit) {
      // 上限即取消，不是只停止消费
      teardownSearchTask(runtime)
      session.searchStatus = 'limited'
    }
  }

  function scheduleSearchFlush(session: Session, runtime: SearchRuntime) {
    if (runtime.cancelFlush) return
    runtime.cancelFlush = scheduleFrame(() => {
      runtime.cancelFlush = null
      flushSearchItems(session, runtime)
    })
  }

  /**
   * 精确 key 快速通道（doc/frontend/key-tree.md「精确命中」）：候选可能就是完整 key 名，在扫描任务之外并行
   * 探测一次（getKeyMeta 一次 IPC，keyType 为 null 即不存在）。
   * 元字符判断在调用方：作用域下候选是「前缀 + 分隔符 + 词」，前缀是树里
   * 的字面路径，元字符只看词。明确放弃的边缘：key 名本身含 glob 元字符
   * 且原样输入时被当作模式，字面命中靠扫描兜底。
   */
  function probeExactKey(session: Session, runtime: SearchRuntime, epoch: number, key: string) {
    void redis
      .getKeyMeta(session.id, key)
      .then((meta) => {
        if (runtime.epoch !== epoch || meta.keyType === null) return
        session.searchExact = { key, keyType: meta.keyType }
        // 与流式结果去重：置顶行是唯一呈现。已经流进来的（keys / 未 flush 的
        // 缓冲）摘掉，还没来的用 seen 挡住 —— 别出现两行
        runtime.seen.add(key)
        runtime.buffer = runtime.buffer.filter((entry) => entry.key !== key)
        const index = session.keys.findIndex((entry) => entry.key === key)
        if (index >= 0) {
          session.keys.splice(index, 1)
          rebuildTree(session)
        }
      })
      .catch(() => undefined) // 探测失败不打扰扫描主线，无非少一行置顶
  }

  /**
   * 搜索框有输入 → 启动 search_keys 任务，key 流式并入本会话的树。
   *
   * rawQuery 是搜索框原文：含 `*` 原样下发，否则包 `*词*` —— 与旧的单批
   * rescan 路径同一条规则；前缀作用域只对前缀转义，词照旧。
   * task_id 就是代际令牌（doc/frontend/key-tree.md「task_id 就是代际令牌」）：每次搜索开新任务，旧任务未消费完的
   * 事件带着旧 id 到达时已无人认领（退订即丢弃）。
   */
  async function startSearch(sessionId: string, rawQuery: string): Promise<void> {
    const session = sessions.get(sessionId)
    if (!session) return
    const query = rawQuery.trim()
    session.searchQuery = query
    // 词空且没有前缀作用域才回浏览态；有作用域时空词就是「扫描此前缀」
    // 的搜索态效果，照常走任务
    if (!query && !session.searchScope) return rescan(sessionId, '')

    const runtime = searchRuntimeOf(sessionId)
    teardownSearchTask(runtime)
    // 搜索态即将清空 keys：还在往里灌的前缀扫描一并拆掉 —— 两条管线不许
    // 同时写一个数组；权威集随基础视图一起作废
    cancelPrefixScans(sessionId)
    authoritativePrefixes.get(sessionId)?.clear()
    const epoch = runtime.epoch
    runtime.seen = new Set()

    // 清空 keys 前先作废在途的浏览扫描，同 rescan 的竞态（见 Session.scanGeneration）：
    // 否则旧 pattern 的那批 key 会推进刚清空的数组
    session.scanGeneration += 1
    session.scanning = false
    session.pattern = scopedSearchPattern(session.searchScope ?? '', session.separator, query)
    session.keys = []
    session.cursor = '0'
    session.scanComplete = false
    session.truncated = false
    session.error = null
    session.searchStatus = 'running'
    session.searchProgress = null
    session.searchExact = null
    rebuildTree(session)

    // 精确探测：作用域下候选是「前缀 + 分隔符 + 词」——
    // 前缀是字面路径，glob 元字符只看词
    if (query && !GLOB_META.test(query)) {
      probeExactKey(
        session,
        runtime,
        epoch,
        session.searchScope ? `${session.searchScope}${session.separator}${query}` : query,
      )
    }

    try {
      const started = await redis.startSearchKeys(sessionId, {
        pattern: session.pattern,
        // 任务 COUNT 独立于浏览路径的 scanCount、单独可调：带 MATCH 时
        // 命中稀疏、回包小，大 COUNT 纯粹省往返（doc/backend/tasks.md「扫到底的扫描任务」），
        // 不要拿浏览路径的 500 盖过它
        count: settings.state.taskScanCount,
        withTtl: settings.state.treeTtl,
      })
      if (runtime.epoch !== epoch) {
        // 等待建任务期间用户又改了输入：这个任务没有消费者，立刻取消
        void redis.cancelTask(started.taskId).catch(() => undefined)
        return
      }
      runtime.taskId = started.taskId
      // 启动 invoke 在途期错过了暂停广播：按当前期望态补发。恢复态（false）
      // 不用发 —— 后端任务的初始态本来就是未暂停。代际检查刚通过，
      // 这里绝不会给已拆掉的任务补发
      if (scanTasksPaused) void redis.setTaskPaused(started.taskId, true).catch(() => undefined)
      runtime.unsubscribe = tasks.subscribe(started.taskId, (items, event) => {
        if (runtime.epoch !== epoch) return
        // 估算随事件到达（后端已做单调不减）；MATCH 稀疏时事件可能
        // 只有估算、没有条目 —— tasks store 对这类空页事件也会转发
        if (event.estimatedProgress !== null) session.searchProgress = event.estimatedProgress
        for (const item of items) {
          if (item.type !== 'key' || runtime.seen.has(item.key)) continue
          runtime.seen.add(item.key)
          runtime.buffer.push({ key: item.key, keyType: item.keyType, ttl: item.ttl })
        }
        scheduleSearchFlush(session, runtime)
      })
      void tasks.watchSettled(started.taskId).then((summary) => {
        if (runtime.epoch !== epoch) return
        // 进终态前把缓冲清账，否则最后一页会留在缓冲里
        runtime.cancelFlush?.()
        runtime.cancelFlush = null
        flushSearchItems(session, runtime)
        // flush 可能触发上限路径（翻代际 + 置 limited），那就不再落终态
        if (runtime.epoch !== epoch || session.searchStatus !== 'running') return
        // 终态落账时对称清理 runtime（与前缀路径一致）：任务已经终结，
        // 先摘 taskId 再 teardown 就不会对它多发一次取消；退订与缓冲
        // 一并清掉，下一次搜索不再对着已终结的任务补发 cancelTask
        runtime.taskId = null
        teardownSearchTask(runtime)
        if (summary.status === 'completed') {
          // 扫完：这个 pattern 的结果是权威的，scanComplete 让计数摘掉 `+`，
          // 空结果的文案也随之变成「没有匹配的 Key」
          session.searchStatus = 'completed'
          session.scanComplete = true
        } else if (summary.status === 'failed') {
          session.searchStatus = 'failed'
          session.error = new AppFault({
            code: 'task.search_failed',
            message: summary.message ?? '搜索任务失败',
            detail: null,
            recoverable: true,
          })
        } else if (summary.status === 'cancelled') {
          // 不是本 store 取消的（自家取消都先翻代际）——比如状态栏的停止按钮
          session.searchStatus = 'cancelled'
        }
      })
    } catch (error) {
      if (runtime.epoch !== epoch) return
      session.searchStatus = 'failed'
      session.error = toFault(error)
    }
  }

  /** 取消进行中的搜索（取消按钮 / 切走会话）。已找到的结果保留，状态如实标成已取消。 */
  function cancelSearch(sessionId: string) {
    const session = sessions.get(sessionId)
    if (!session || session.searchStatus !== 'running') return
    teardownSearchTask(searchRuntimeOf(sessionId))
    session.searchStatus = 'cancelled'
  }

  /**
   * 右键 namespace →「在此搜索」。搜索框获得该前缀作用域，
   * 带当前词立即重搜（词空即「扫描此前缀」的搜索态效果）——
   * 复用 startSearch 整条管线：文案矩阵、取消时机、上限、暂停都不另写。
   */
  async function searchInPrefix(sessionId: string, prefix: string) {
    const session = sessions.get(sessionId)
    if (!session || !prefix) return
    session.searchScope = prefix
    await startSearch(sessionId, session.searchQuery)
  }

  /** 清除前缀 chip：词还在就退回全局搜索，词空就回浏览态。 */
  async function clearSearchScope(sessionId: string) {
    const session = sessions.get(sessionId)
    if (!session || session.searchScope === null) return
    session.searchScope = null
    if (session.searchQuery) await startSearch(sessionId, session.searchQuery)
    else await rescan(sessionId, '')
  }

  /**
   * 窗口隐藏 → 暂停所有进行中的扫描类任务（搜索 + 前缀扫描）；可见 → 恢复
   * （doc/frontend/key-tree.md「取消时机」）。暂停不是取消：后端在页间等待，恢复后从当前游标继续。
   * 批量删除这类任务不受影响 —— 藏在后台跑完才是用户要的。
   */
  function setSearchTasksPaused(paused: boolean) {
    scanTasksPaused = paused
    // 转入隐藏时先同步清账合帧缓冲：rAF 在隐藏窗口里不再跑，暂停生效前
    // 在途的最后一两页会滞留 buffer，直到窗口重新可见才 flush。
    // 先 flush 再发暂停 —— flush 可能触发上限路径把任务取消掉，
    // 被取消的任务不该再收一条暂停
    if (paused) flushScanBuffers()
    for (const [sessionId, runtime] of searchRuntimes) {
      if (!runtime.taskId) continue
      if (sessions.get(sessionId)?.searchStatus !== 'running') continue
      void redis.setTaskPaused(runtime.taskId, paused).catch(() => undefined)
    }
    for (const runtimes of prefixRuntimes.values()) {
      for (const runtime of runtimes.values()) {
        if (runtime.taskId) void redis.setTaskPaused(runtime.taskId, paused).catch(() => undefined)
      }
    }
  }

  /** 同步清账所有在途的合帧缓冲（搜索 + 前缀扫描）。只在转入隐藏时调用。 */
  function flushScanBuffers() {
    for (const [sessionId, runtime] of searchRuntimes) {
      if (!runtime.cancelFlush) continue
      const session = sessions.get(sessionId)
      if (!session) continue
      runtime.cancelFlush()
      runtime.cancelFlush = null
      flushSearchItems(session, runtime)
    }
    for (const [sessionId, runtimes] of prefixRuntimes) {
      const session = sessions.get(sessionId)
      if (!session) continue
      // flushPrefixItems 触到上限会 teardown 并从 runtimes 里删自己，快照后遍历
      for (const [prefix, runtime] of Array.from(runtimes)) {
        if (!runtime.cancelFlush) continue
        runtime.cancelFlush()
        runtime.cancelFlush = null
        flushPrefixItems(sessionId, prefix, session, runtime)
      }
    }
  }

  /* ── 前缀扫描与前缀权威（doc/frontend/key-tree.md「文件夹内搜索与前缀权威」）──────────── */

  function prefixRuntimesOf(sessionId: string): Map<string, PrefixScanRuntime> {
    let runtimes = prefixRuntimes.get(sessionId)
    if (!runtimes) {
      runtimes = new Map()
      prefixRuntimes.set(sessionId, runtimes)
    }
    return runtimes
  }

  /** 取回 reactive 代理而不是刚 new 出来的裸 Set —— 对裸 Set 的改动不触发依赖。 */
  function authoritySetOf(sessionId: string): Set<string> {
    if (!authoritativePrefixes.has(sessionId)) authoritativePrefixes.set(sessionId, new Set())
    return authoritativePrefixes.get(sessionId)!
  }

  function scanningSetOf(sessionId: string): Set<string> {
    if (!scanningPrefixes.has(sessionId)) scanningPrefixes.set(sessionId, new Set())
    return scanningPrefixes.get(sessionId)!
  }

  /**
   * 树上某个前缀的计数是否精确（可以摘 `+`）：全库扫完，或被权威集覆盖。
   * 覆盖对子前缀也成立：`user` 权威则 `user:profile` 也权威。
   */
  function isCountExact(sessionId: string, path: string): boolean {
    const session = sessions.get(sessionId)
    if (!session) return false
    if (session.scanComplete) return true
    const set = authoritativePrefixes.get(sessionId)
    return !!set && isPrefixCovered(set, path, session.separator)
  }

  /** 该前缀是否正在扫描（计数位换 spinner 用）。 */
  function isPrefixScanning(sessionId: string, prefix: string): boolean {
    return scanningPrefixes.get(sessionId)?.has(prefix) ?? false
  }

  /**
   * 「扫描此前缀」是否可用：浏览态、计数不精确、没在扫、树还有余量。
   * 搜索态下不开这个入口 —— 不允许两条管线同时往一个会话灌 key；
   * 树已到上限时再扫也只会被立刻取消，不如从一开始就不可点。
   */
  function canScanPrefix(sessionId: string, prefix: string): boolean {
    const session = sessions.get(sessionId)
    if (!session || !prefix) return false
    if (session.searchStatus !== 'idle') return false
    if (session.keys.length >= settings.state.treeNodeLimit) return false
    return !isCountExact(sessionId, prefix) && !isPrefixScanning(sessionId, prefix)
  }

  /**
   * 拆掉一个前缀扫描：翻代际、退订、丢缓冲、通知后端取消、清进行中标记。
   * **绝不置权威** —— 那是 completed 专属的落点。
   */
  function teardownPrefixScan(sessionId: string, prefix: string, runtime: PrefixScanRuntime) {
    runtime.epoch += 1
    runtime.unsubscribe?.()
    runtime.unsubscribe = null
    runtime.cancelFlush?.()
    runtime.cancelFlush = null
    runtime.buffer = []
    if (runtime.taskId) {
      void redis.cancelTask(runtime.taskId).catch(() => undefined)
      runtime.taskId = null
    }
    prefixRuntimes.get(sessionId)?.delete(prefix)
    scanningPrefixes.get(sessionId)?.delete(prefix)
  }

  /** 取消一个会话的全部前缀扫描（进搜索态 / rescan / 关会话时）。 */
  function cancelPrefixScans(sessionId: string) {
    const runtimes = prefixRuntimes.get(sessionId)
    if (!runtimes) return
    for (const [prefix, runtime] of Array.from(runtimes)) {
      teardownPrefixScan(sessionId, prefix, runtime)
    }
  }

  /**
   * 前缀扫描的合帧并入：按 key 名去重后 push 进 session.keys 重建树。
   * 触到 treeNodeLimit 与搜索同一条规则（doc/frontend/key-tree.md「前缀权威」）：取消任务、不置权威 ——
   * 没并进来的部分让这个前缀依然不完整，摘 `+` 就是撒谎。
   */
  function flushPrefixItems(
    sessionId: string,
    prefix: string,
    session: Session,
    runtime: PrefixScanRuntime,
  ) {
    const limit = settings.state.treeNodeLimit
    if (runtime.buffer.length) {
      if (session.keys.length !== runtime.knownLength) {
        // 浏览续扫 / 删除 / rememberKey 动过 keys：去重基线重建
        runtime.seen = new Set(session.keys.map((entry) => entry.key))
      }
      const fresh: KeyEntry[] = []
      for (const entry of runtime.buffer) {
        if (session.keys.length + fresh.length >= limit) break
        if (runtime.seen.has(entry.key)) continue
        runtime.seen.add(entry.key)
        fresh.push(entry)
      }
      runtime.buffer = []
      if (fresh.length) {
        session.keys.push(...fresh)
        session.truncated = session.keys.length >= limit
        rebuildTree(session)
      }
      runtime.knownLength = session.keys.length
    }
    if (session.keys.length >= limit && runtime.taskId) {
      teardownPrefixScan(sessionId, prefix, runtime)
    }
  }

  function schedulePrefixFlush(
    sessionId: string,
    prefix: string,
    session: Session,
    runtime: PrefixScanRuntime,
  ) {
    if (runtime.cancelFlush) return
    runtime.cancelFlush = scheduleFrame(() => {
      runtime.cancelFlush = null
      flushPrefixItems(sessionId, prefix, session, runtime)
    })
  }

  /**
   * 「扫描此前缀」：把一个前缀扫到底（点 `N+` 徽标 / 右键「扫描此前缀」）。
   *
   * 这是**浏览态的叠加层**，不是搜索：树不进搜索态，结果流式并入
   * session.keys（load_all_keys 任务）。完成即把前缀记进权威集，
   * 取消 / 失败只清进行中标记。搜索态下 canScanPrefix 直接弹回 ——
   * 界面上入口也已禁用，这里是守卫兜底。
   */
  async function scanPrefix(sessionId: string, prefix: string) {
    const session = sessions.get(sessionId)
    if (!session || !canScanPrefix(sessionId, prefix)) return

    const runtime: PrefixScanRuntime = {
      epoch: 0,
      taskId: null,
      unsubscribe: null,
      cancelFlush: null,
      buffer: [],
      seen: new Set(session.keys.map((entry) => entry.key)),
      knownLength: session.keys.length,
    }
    prefixRuntimesOf(sessionId).set(prefix, runtime)
    scanningSetOf(sessionId).add(prefix)
    const epoch = runtime.epoch

    try {
      const started = await redis.startLoadAllKeys(sessionId, {
        pattern: matchPatternFor(prefix, session.separator),
        // 与搜索同一份任务 COUNT 设置，理由见 startSearch
        count: settings.state.taskScanCount,
        withTtl: settings.state.treeTtl,
      })
      if (runtime.epoch !== epoch) {
        // 等待建任务期间扫描被作废（rescan / 进搜索态）：任务没有消费者，立刻取消
        void redis.cancelTask(started.taskId).catch(() => undefined)
        return
      }
      runtime.taskId = started.taskId
      // 启动在途期错过暂停广播：按期望态补发，同 startSearch
      if (scanTasksPaused) void redis.setTaskPaused(started.taskId, true).catch(() => undefined)
      runtime.unsubscribe = tasks.subscribe(started.taskId, (items) => {
        if (runtime.epoch !== epoch) return
        for (const item of items) {
          if (item.type !== 'key') continue
          runtime.buffer.push({ key: item.key, keyType: item.keyType, ttl: item.ttl })
        }
        schedulePrefixFlush(sessionId, prefix, session, runtime)
      })
      void tasks.watchSettled(started.taskId).then((summary) => {
        if (runtime.epoch !== epoch) return
        // 进终态前把缓冲清账；flush 可能因上限把任务拆掉（翻代际），那就不置权威
        runtime.cancelFlush?.()
        runtime.cancelFlush = null
        flushPrefixItems(sessionId, prefix, session, runtime)
        if (runtime.epoch !== epoch) return
        // 任务已经在终态，teardown 不必再发一次取消
        runtime.taskId = null
        teardownPrefixScan(sessionId, prefix, runtime)
        if (summary.status === 'completed') {
          authoritySetOf(sessionId).add(prefix)
        } else if (summary.status === 'failed') {
          session.error = new AppFault({
            code: 'task.prefix_scan_failed',
            message: summary.message ?? '前缀扫描任务失败',
            detail: null,
            recoverable: true,
          })
        }
      })
    } catch (error) {
      if (runtime.epoch !== epoch) return
      teardownPrefixScan(sessionId, prefix, runtime)
      session.error = toFault(error)
    }
  }

  /** 写操作之后局部同步一个 key，避免整棵树重扫。 */
  function forgetKey(sessionId: string, key: string) {
    const session = sessions.get(sessionId)
    if (!session) return
    const index = session.keys.findIndex((entry) => entry.key === key)
    if (index >= 0) {
      session.keys.splice(index, 1)
      rebuildTree(session)
    }
  }

  /**
   * `forgetKey` 的复数版：批量删除一个前缀之后，把树里那一片一起摘掉。
   *
   * **判据是 `startsWith` 而不是 glob 匹配。** 树入口拼模式时
   * `matchPatternFor` 已经把 key 名里的元字符转义掉了，所以
   * `前缀 + 分隔符 + *` 的语义**就是**「以这个字面前缀开头」——
   * `startsWith` 与它精确等价。在前端再写一个 glob 匹配器就是第二份实现，
   * 两边对 `[]` 的口径迟早会漂，而漂的后果是树上留下已经不存在的 key。
   *
   * 重建整棵树不便宜，所以真的少了东西才重建。
   */
  function forgetPrefix(sessionId: string, prefix: string) {
    // `startsWith('')` 恒真：空前缀会把这个会话已加载的 key 全裁掉，
    // 而想清空的调用方要的是 `rescan`。store 的公开函数不该指望调用方守规矩。
    if (!prefix) return
    const session = sessions.get(sessionId)
    if (!session) return
    const kept = session.keys.filter((entry) => !entry.key.startsWith(prefix))
    if (kept.length === session.keys.length) return
    session.keys = kept
    rebuildTree(session)
  }

  function rememberKey(sessionId: string, key: string, keyType: string | null) {
    const session = sessions.get(sessionId)
    if (!session) return
    if (session.keys.some((entry) => entry.key === key)) return
    session.keys.push({ key, keyType, ttl: null })
    rebuildTree(session)
  }

  /**
   * 后端运行时降级了某项能力（doc/backend/capabilities.md「运行时降级」）。
   * 会话已关掉就丢弃 —— 事件可能晚于 closeSession 到达。
   */
  function applyCapabilityChange(event: CapabilityChangeEvent) {
    const session = sessions.get(event.connectionId)
    if (!session) return
    session.capabilities[event.id] = event.capability
  }

  /* ── 展开态 ───────────────────────────────────────────── */

  function toggleConnection(connectionKey: string) {
    if (expandedConnections.has(connectionKey)) expandedConnections.delete(connectionKey)
    else expandedConnections.add(connectionKey)
  }

  function toggleDb(connectionKey: string, db: number) {
    const id = sessionKeyOf(connectionKey, db)
    if (expandedDbs.has(id)) expandedDbs.delete(id)
    else expandedDbs.add(id)
  }

  function isDbExpanded(connectionKey: string, db: number) {
    return expandedDbs.has(sessionKeyOf(connectionKey, db))
  }

  function sessionIdFor(connectionKey: string, db: number): string | null {
    return openedDbs.get(sessionKeyOf(connectionKey, db)) ?? null
  }

  return {
    sessions,
    dbLists,
    expandedConnections,
    expandedDbs,
    expandedPaths,
    selected,
    activeSessionId,
    activeSession,
    treeCache,

    connecting,
    isConnecting,

    pathsOf,
    treeOf,
    draftOf,
    invalidateDraft,
    openConnection,
    openDb,
    closeSession,
    closeConnection,
    loadDbList,
    scanMore,
    rescan,
    startSearch,
    cancelSearch,
    searchInPrefix,
    clearSearchScope,
    scanPrefix,
    isCountExact,
    isPrefixScanning,
    canScanPrefix,
    setSearchTasksPaused,
    forgetKey,
    forgetPrefix,
    rememberKey,
    applyCapabilityChange,
    toggleConnection,
    toggleDb,
    isDbExpanded,
    sessionIdFor,
    sessionIdsFor,
    revisionOf,
    invalidateKeyspace,
  }
})
