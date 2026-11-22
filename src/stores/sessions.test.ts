import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import * as redis from '@/api/redis'
import { getConnection } from '@/api/connections'
import { onTaskProgress } from '@/api/events'
import type {
  Capability,
  ConnectionEditDraft,
  ConnectionOpened,
  KeyEntry,
  KeyListResult,
  KeyMeta,
  TaskProgressEvent,
  TaskStarted,
  TaskStatus,
  TaskStatusSnapshot,
} from '@/types/ipc'
import { useSessionsStore } from './sessions'
import { useSettingsStore } from './settings'
import { useTasksStore } from './tasks'

/*
 * 会话 store 的一切网络行为都走 `@/api/*`，把这一层 mock 掉之后，
 * 时序就完全由测试端的 deferred promise 控制 —— 能够精确复现
 * 「rescan 之后旧扫描的响应才到达」这一步，这是竞态测试的全部前提。
 */
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

vi.mock('@/api/redis', () => ({
  openStoredConnection: vi.fn(),
  closeConnection: vi.fn(),
  getDatabaseList: vi.fn(),
  scanKeys: vi.fn(),
  startSearchKeys: vi.fn(),
  startLoadAllKeys: vi.fn(),
  cancelTask: vi.fn(),
  getTaskStatus: vi.fn(),
  getKeyMeta: vi.fn(),
  setTaskPaused: vi.fn(),
}))

vi.mock('@/api/connections', () => ({
  getConnection: vi.fn(),
}))

/* 任务事件不走真 Tauri 频道：捕获 tasks store 注册的 handler，
   测试端直接投递 TaskProgressEvent —— 时序照样完全可控。 */
vi.mock('@/api/events', () => ({
  onTaskProgress: vi.fn(),
}))

/* settings / connections store 在创建时就摸 localStorage 与 document，
   vitest 的 node 环境里两者都不存在，给最小桩即可 —— 测试不关心外观。 */
vi.stubGlobal('localStorage', { getItem: () => null, setItem: () => {} })
vi.stubGlobal('document', {
  documentElement: { dataset: {}, style: { setProperty: () => {} } },
})
/* 建连结果里的身份与能力。这些测试不关心它们，给一份 Redis 7 的基线就行。 */
const supported = (reason: string): Capability => ({ state: 'supported', usable: true, reason })
const opened = {
  connectionId: '',
  connectionName: 'test',
  readonly: false,
  server: { flavor: 'redis', version: '7.2.4', compatVersion: '7.2.4' },
  capabilities: {
    expireConditions: supported('Redis 7.2.4，条件设置可用'),
    setKeepTtl: supported('Redis 7.2.4，保存时保留过期时间（KEEPTTL）可用'),
    scripting: supported('服务端提供 `EVAL`，Lua 脚本可用'),
  },
} satisfies ConnectionOpened

/* tasks store 的终态清理用 window.setTimeout */
vi.stubGlobal('window', {
  setTimeout: globalThis.setTimeout.bind(globalThis),
  clearTimeout: globalThis.clearTimeout.bind(globalThis),
})
/* 搜索的合帧走 requestAnimationFrame：node 里没有，退成宏任务，
   与下面 flush() 的「隔一个宏任务」节奏正好咬合。 */
vi.stubGlobal('requestAnimationFrame', (callback: (time: number) => void) => {
  setTimeout(() => callback(0), 0)
  return 1
})
vi.stubGlobal('cancelAnimationFrame', () => {})

const SESSION_ID = 'sess-1'

const draft: ConnectionEditDraft = {
  key: 'conn-a',
  name: 'test',
  host: '127.0.0.1',
  port: 6379,
  username: null,
  db: 0,
  separator: ':',
  connectionReadOnly: null,
  cluster: false,
  color: null,
  order: null,
  auth: null,
  sslOptions: null,
  sshOptions: null,
  sentinelOptions: null,
  hasAuth: false,
  hasSshPassword: false,
  hasSshPassphrase: false,
  hasSentinelNodePassword: false,
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

function entry(key: string): KeyEntry {
  return { key, keyType: 'string', ttl: null }
}

/** 让在途 promise 的 then/catch 链走完（微任务不够，隔一个宏任务最稳）。 */
async function flush() {
  await new Promise<void>((resolve) => setTimeout(resolve, 0))
}

/** 打开会话，但让初扫挂起在途，把控制权交给测试。 */
async function openWithHangingScan() {
  const store = useSessionsStore()
  const firstScan = deferred<KeyListResult>()
  vi.mocked(redis.scanKeys).mockReturnValueOnce(firstScan.promise)
  const opening = store.openDb('conn-a', 0)
  await vi.waitFor(() => {
    expect(redis.scanKeys).toHaveBeenCalledTimes(1)
  })
  const session = store.sessions.get(SESSION_ID)!
  return { store, session, firstScan, opening }
}

describe('rescan 的扫描竞态（Session.scanGeneration）', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
    vi.mocked(getConnection).mockResolvedValue(draft)
    vi.mocked(redis.openStoredConnection).mockResolvedValue({ ...opened, connectionId: SESSION_ID })
    vi.mocked(redis.getDatabaseList).mockResolvedValue([{ index: 0, keys: 2 }])
    vi.mocked(redis.closeConnection).mockResolvedValue(undefined)
  })

  it('初扫在途时 rescan：立即发出新扫描，旧响应整体丢弃', async () => {
    const { store, session, firstScan, opening } = await openWithHangingScan()

    const secondScan = deferred<KeyListResult>()
    vi.mocked(redis.scanKeys).mockReturnValueOnce(secondScan.promise)
    const rescanning = store.rescan(SESSION_ID, 'new*')

    // 修复前这一步不成立：scanMore 被 scanning 守卫弹回，新扫描根本没发出
    await vi.waitFor(() => {
      expect(redis.scanKeys).toHaveBeenCalledTimes(2)
    })
    const [, cursor, pattern] = vi.mocked(redis.scanKeys).mock.calls[1]
    expect(cursor).toBe('0')
    expect(pattern).toBe('new*')

    // 旧扫描此刻才回来：key 不许进刚清空的数组，游标不许拨到旧续点
    firstScan.resolve({ keys: [entry('old:1')], cursor: '42', completed: false })
    await flush()
    expect(session.keys).toHaveLength(0)
    expect(session.cursor).toBe('0')
    expect(session.scanComplete).toBe(false)
    // 旧响应也不能把新代际正在进行的扫描标成未进行
    expect(session.scanning).toBe(true)

    // 新扫描的响应照常落账
    secondScan.resolve({ keys: [entry('new:1')], cursor: '0', completed: true })
    await rescanning
    await opening
    expect(session.keys.map((k) => k.key)).toEqual(['new:1'])
    expect(session.scanComplete).toBe(true)
    expect(session.scanning).toBe(false)
  })

  it('旧代际的失败同样丢弃，不写 error', async () => {
    const { store, session, firstScan, opening } = await openWithHangingScan()

    const secondScan = deferred<KeyListResult>()
    vi.mocked(redis.scanKeys).mockReturnValueOnce(secondScan.promise)
    const rescanning = store.rescan(SESSION_ID, 'new*')
    await vi.waitFor(() => {
      expect(redis.scanKeys).toHaveBeenCalledTimes(2)
    })

    // 旧扫描以失败告终 —— 它报告的是没人关心的那次扫描，不该冒充新扫描的错误
    firstScan.reject(new Error('stale failure'))
    await flush()
    expect(session.error).toBeNull()
    expect(session.scanning).toBe(true)

    secondScan.resolve({ keys: [], cursor: '0', completed: true })
    await rescanning
    await opening
    expect(session.error).toBeNull()
  })
})

/* ── 搜索 = 扫到底的任务（doc/frontend/key-tree.md「搜索」）──────────────── */

/** 测试端捕获的任务事件入口，等价于后端往 redis://task-progress 发一条。 */
let emitTask: (event: TaskProgressEvent) => void = () => {}

function taskEvent(taskId: string, keys: string[], status: TaskStatus = 'progress'): TaskProgressEvent {
  return {
    taskId,
    connectionId: SESSION_ID,
    connectionName: 'test',
    taskType: 'search_keys',
    status,
    processed: keys.length,
    succeeded: keys.length,
    failed: 0,
    total: null,
    estimatedProgress: null,
    items: keys.map((key) => ({ type: 'key' as const, key, keyType: 'string', ttl: null })),
    message: null,
  }
}

function cancelledSnapshot(taskId: string): TaskStatusSnapshot {
  return {
    taskId,
    connectionId: SESSION_ID,
    connectionName: 'test',
    taskType: 'search_keys',
    status: 'cancelled',
    processed: 0,
    succeeded: 0,
    failed: 0,
    total: null,
    estimatedProgress: null,
    message: null,
  }
}

/** watchSettled 的兜底查询（tasks store）用：任务还在跑，兜底不抢答。 */
function runningSnapshot(taskId: string): TaskStatusSnapshot {
  return { ...cancelledSnapshot(taskId), status: 'progress' }
}

/** getKeyMeta 的未命中形状：keyType 为 null 即 key 不存在。 */
function missMeta(key: string): KeyMeta {
  return { key, keyType: null, ttl: -2, size: null, encoding: null, memory: null, slot: null, hll: null }
}

function keyNames(session: { keys: KeyEntry[] }): string[] {
  return session.keys.map((entry) => entry.key)
}

describe('搜索任务的消费', () => {
  beforeEach(async () => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
    vi.mocked(getConnection).mockResolvedValue(draft)
    vi.mocked(redis.openStoredConnection).mockResolvedValue({ ...opened, connectionId: SESSION_ID })
    vi.mocked(redis.getDatabaseList).mockResolvedValue([{ index: 0, keys: 2 }])
    vi.mocked(redis.closeConnection).mockResolvedValue(undefined)
    vi.mocked(redis.cancelTask).mockImplementation(async (taskId) => cancelledSnapshot(taskId))
    vi.mocked(redis.getTaskStatus).mockImplementation(async (taskId) => runningSnapshot(taskId))
    vi.mocked(redis.setTaskPaused).mockImplementation(async (taskId) => runningSnapshot(taskId))
    vi.mocked(redis.getKeyMeta).mockImplementation(async (_id, key) => missMeta(key))
    vi.mocked(onTaskProgress).mockImplementation(async (handler) => {
      emitTask = handler
      return () => {}
    })
    // 事件管线在应用里由 App.vue 启动，测试里手动接上
    await useTasksStore().start()
  })

  /** 打开会话（初扫立即完成），随后把它切进搜索态。 */
  async function openSearching(taskId = 'task-a', query = 'foo') {
    const store = useSessionsStore()
    vi.mocked(redis.scanKeys).mockResolvedValueOnce({ keys: [], cursor: '0', completed: true })
    await store.openDb('conn-a', 0)
    const session = store.sessions.get(SESSION_ID)!
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId })
    await store.startSearch(SESSION_ID, query)
    return { store, session }
  }

  it('task_id 即代际令牌：旧任务的事件整体丢弃，新任务照常落账', async () => {
    const { store, session } = await openSearching('task-a', 'foo')
    expect(session.searchStatus).toBe('running')
    expect(vi.mocked(redis.startSearchKeys).mock.calls[0][1]).toEqual({
      pattern: '*foo*',
      count: 2000,
      withTtl: false,
    })

    emitTask(taskEvent('task-a', ['foo:1']))
    await flush()
    expect(keyNames(session)).toEqual(['foo:1'])

    // 新搜索 = 新任务；旧任务被取消，树清空
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-b' })
    await store.startSearch(SESSION_ID, 'bar')
    expect(redis.cancelTask).toHaveBeenCalledWith('task-a')
    expect(session.keys).toHaveLength(0)

    // 旧任务未消费完的事件此刻才到达：带着旧 task_id，无人认领，整体丢弃
    emitTask(taskEvent('task-a', ['foo:2']))
    await flush()
    expect(session.keys).toHaveLength(0)

    emitTask(taskEvent('task-b', ['bar:1']))
    await flush()
    expect(keyNames(session)).toEqual(['bar:1'])
  })

  it('并入按 key 名去重：SCAN 在 rehash 期间会重复返回同一 key', async () => {
    const { session } = await openSearching()

    emitTask(taskEvent('task-a', ['a:1', 'a:1', 'a:2']))
    emitTask(taskEvent('task-a', ['a:2', 'a:3']))
    await flush()
    expect(keyNames(session)).toEqual(['a:1', 'a:2', 'a:3'])
  })

  it('触到 treeNodeLimit：前端主动取消任务并进入已达上限态', async () => {
    const settings = useSettingsStore()
    settings.state.treeNodeLimit = 3
    const { session } = await openSearching()

    emitTask(taskEvent('task-a', ['k:1', 'k:2', 'k:3', 'k:4']))
    await flush()
    // 只停止消费不取消是不允许的：服务端会继续全速扫、喂给丢弃结果的客户端
    expect(redis.cancelTask).toHaveBeenCalledWith('task-a')
    expect(session.searchStatus).toBe('limited')
    expect(session.keys).toHaveLength(3)

    // 已退订：迟到的事件不再消费
    emitTask(taskEvent('task-a', ['k:5']))
    await flush()
    expect(session.keys).toHaveLength(3)
  })

  it('精确命中与流式结果去重：以置顶行为准，不出现两行', async () => {
    const store = useSessionsStore()
    vi.mocked(redis.scanKeys).mockResolvedValueOnce({ keys: [], cursor: '0', completed: true })
    await store.openDb('conn-a', 0)
    const session = store.sessions.get(SESSION_ID)!

    // 探测晚于第一页到达 —— 已并入树的同名 key 必须被摘下来
    const probe = deferred<KeyMeta>()
    vi.mocked(redis.getKeyMeta).mockReturnValueOnce(probe.promise)
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-a' })
    await store.startSearch(SESSION_ID, 'user:1')

    emitTask(taskEvent('task-a', ['user:1', 'user:10']))
    await flush()
    expect(keyNames(session)).toEqual(['user:1', 'user:10'])

    probe.resolve({ ...missMeta('user:1'), keyType: 'string', ttl: -1 })
    await flush()
    expect(session.searchExact).toEqual({ key: 'user:1', keyType: 'string' })
    expect(keyNames(session)).toEqual(['user:10'])

    // 之后再流进来的同名 key 也被 seen 挡住
    emitTask(taskEvent('task-a', ['user:1']))
    await flush()
    expect(keyNames(session)).toEqual(['user:10'])
  })

  it('估算进度随事件落进会话搜索态，空页事件也送达', async () => {
    const { store, session } = await openSearching()
    expect(session.searchProgress).toBeNull()

    emitTask({ ...taskEvent('task-a', ['foo:1']), estimatedProgress: 0.25 })
    await flush()
    expect(session.searchProgress).toBe(0.25)

    // MATCH 稀疏时整页可能没有条目，只有估算在前进 —— 空页事件也要落账
    emitTask({ ...taskEvent('task-a', []), estimatedProgress: 0.5 })
    await flush()
    expect(session.searchProgress).toBe(0.5)

    // 新搜索 = 新任务：估算归零回不可用，不许带着旧任务的百分比起跑
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-b' })
    await store.startSearch(SESSION_ID, 'bar')
    expect(session.searchProgress).toBeNull()

    // 旧任务迟到的估算带着旧 task_id，无人认领
    emitTask({ ...taskEvent('task-a', []), estimatedProgress: 0.9 })
    await flush()
    expect(session.searchProgress).toBeNull()
  })

  it('任务 completed → 缓冲清账、标记扫完（计数得以摘掉 +）', async () => {
    const { session } = await openSearching()

    emitTask(taskEvent('task-a', ['x:1']))
    emitTask(taskEvent('task-a', [], 'completed'))
    await flush()
    expect(keyNames(session)).toEqual(['x:1'])
    expect(session.searchStatus).toBe('completed')
    expect(session.scanComplete).toBe(true)
  })

  it('事件先于 invoke 响应到达：首页缓冲回放、终态即时兑现', async () => {
    const store = useSessionsStore()
    vi.mocked(redis.scanKeys).mockResolvedValueOnce({ keys: [], cursor: '0', completed: true })
    await store.openDb('conn-a', 0)
    const session = store.sessions.get(SESSION_ID)!

    // 本机小 keyspace 一页扫完：首页 items 与 completed 事件都赶在
    // startSearchKeys 的 TaskStarted 响应之前送达
    const starting = deferred<TaskStarted>()
    vi.mocked(redis.startSearchKeys).mockReturnValueOnce(starting.promise)
    const searching = store.startSearch(SESSION_ID, 'foo')
    emitTask(taskEvent('task-a', ['foo:1', 'foo:2']))
    emitTask(taskEvent('task-a', [], 'completed'))
    starting.resolve({ taskId: 'task-a' })
    await searching
    await flush()

    // 修复前：整页在 sink 注册前被丢弃、终态 waiter 永不 resolve ——
    // 界面永远停在「已找到 0 个 · 扫描中…」
    expect(keyNames(session)).toEqual(['foo:1', 'foo:2'])
    expect(session.searchStatus).toBe('completed')
    expect(session.scanComplete).toBe(true)
  })

  it('搜索态下刷新（不传 pattern）：重启搜索任务，不退回单批扫描', async () => {
    const { store, session } = await openSearching('task-a', 'foo')
    emitTask(taskEvent('task-a', ['foo:1']))
    await flush()
    expect(keyNames(session)).toEqual(['foo:1'])

    // 刷新按钮 / F5 / 右键「重新扫描」都不带 pattern 走 rescan
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-b' })
    await store.rescan(SESSION_ID)

    // 旧任务取消，同一 pattern 的新任务清空重跑，词保留在搜索框里
    expect(redis.cancelTask).toHaveBeenCalledWith('task-a')
    expect(vi.mocked(redis.startSearchKeys).mock.calls[1][1].pattern).toBe('*foo*')
    expect(session.searchStatus).toBe('running')
    expect(session.searchQuery).toBe('foo')
    expect(session.keys).toHaveLength(0)
    // 修复前这里退化成浏览游标扫描：搜索 pattern 配上「加载更多（已加载 N）」。
    // 游标扫描只允许 openDb 初扫那一次
    expect(redis.scanKeys).toHaveBeenCalledTimes(1)

    emitTask(taskEvent('task-b', ['foo:2']))
    await flush()
    expect(keyNames(session)).toEqual(['foo:2'])
  })

  it('任务 COUNT 接设置项：改过的 taskScanCount 透传进任务 options', async () => {
    const settings = useSettingsStore()
    settings.state.taskScanCount = 4321
    await openSearching('task-a', 'foo')
    expect(vi.mocked(redis.startSearchKeys).mock.calls[0][1]).toMatchObject({ count: 4321 })
  })

  it('暂停广播落在任务启动在途期：拿到 taskId 后按期望态补发', async () => {
    const store = useSessionsStore()
    vi.mocked(redis.scanKeys).mockResolvedValueOnce({ keys: [], cursor: '0', completed: true })
    await store.openDb('conn-a', 0)

    const starting = deferred<TaskStarted>()
    vi.mocked(redis.startSearchKeys).mockReturnValueOnce(starting.promise)
    const searching = store.startSearch(SESSION_ID, 'foo')
    // invoke 在途：广播还找不到 taskId，什么都发不出去
    store.setSearchTasksPaused(true)
    expect(redis.setTaskPaused).not.toHaveBeenCalled()

    // 修复前：taskId 到账后没人补发，这个任务在隐藏的窗口里全速跑到底
    starting.resolve({ taskId: 'task-a' })
    await searching
    expect(redis.setTaskPaused).toHaveBeenCalledWith('task-a', true)

    // 期望态是恢复（false）时不补发 —— 后端任务的初始态本来就是未暂停
    const restarting = deferred<TaskStarted>()
    vi.mocked(redis.startSearchKeys).mockReturnValueOnce(restarting.promise)
    const searchingAgain = store.startSearch(SESSION_ID, 'bar')
    store.setSearchTasksPaused(false)
    restarting.resolve({ taskId: 'task-b' })
    await searchingAgain
    expect(redis.setTaskPaused).not.toHaveBeenCalledWith('task-b', true)
  })

  it('启动在途期任务已被作废：只补发取消，不给已拆任务补发暂停', async () => {
    const store = useSessionsStore()
    vi.mocked(redis.scanKeys).mockResolvedValueOnce({ keys: [], cursor: '0', completed: true })
    await store.openDb('conn-a', 0)

    const starting = deferred<TaskStarted>()
    vi.mocked(redis.startSearchKeys).mockReturnValueOnce(starting.promise)
    const searching = store.startSearch(SESSION_ID, 'foo')
    store.setSearchTasksPaused(true)
    // 等待建任务期间用户清空搜索回浏览态：这个任务已无消费者
    vi.mocked(redis.scanKeys).mockResolvedValueOnce({ keys: [], cursor: '0', completed: true })
    await store.rescan(SESSION_ID, '')
    starting.resolve({ taskId: 'task-a' })
    await searching

    expect(redis.cancelTask).toHaveBeenCalledWith('task-a')
    expect(redis.setTaskPaused).not.toHaveBeenCalledWith('task-a', true)
  })

  it('搜索终态清理 runtime：下一次搜索不再对已终结任务补发取消（对称于前缀路径）', async () => {
    const { store } = await openSearching('task-a', 'foo')
    emitTask(taskEvent('task-a', [], 'completed'))
    await flush()

    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-b' })
    await store.startSearch(SESSION_ID, 'bar')
    expect(redis.cancelTask).not.toHaveBeenCalledWith('task-a')
  })

  it('外部取消（状态栏停止按钮）同样清理 runtime：清空搜索不再补刀', async () => {
    const { store, session } = await openSearching('task-a', 'foo')
    emitTask(taskEvent('task-a', [], 'cancelled'))
    await flush()
    expect(session.searchStatus).toBe('cancelled')

    vi.mocked(redis.scanKeys).mockResolvedValueOnce({ keys: [], cursor: '0', completed: true })
    await store.rescan(SESSION_ID, '')
    expect(redis.cancelTask).not.toHaveBeenCalledWith('task-a')
  })

  it('窗口隐藏：rAF 停摆，暂停处理里同步清账合帧缓冲', async () => {
    const { store, session } = await openSearching()
    emitTask(taskEvent('task-a', ['foo:1']))
    // 不等合帧宏任务 —— 隐藏后 rAF 不会再跑，flush 必须在这里同步发生，
    // 否则暂停生效前在途的最后一页滞留缓冲，直到窗口重新可见
    store.setSearchTasksPaused(true)
    expect(keyNames(session)).toEqual(['foo:1'])
    expect(redis.setTaskPaused).toHaveBeenCalledWith('task-a', true)
  })

  it('取消按钮：任务取消、结果保留、状态如实标已取消', async () => {
    const { store, session } = await openSearching()

    emitTask(taskEvent('task-a', ['foo:1']))
    await flush()
    store.cancelSearch(SESSION_ID)
    expect(redis.cancelTask).toHaveBeenCalledWith('task-a')
    expect(session.searchStatus).toBe('cancelled')
    expect(keyNames(session)).toEqual(['foo:1'])
  })

  it('清空搜索回浏览态：先取消任务，再按原逻辑重扫', async () => {
    const { store, session } = await openSearching()

    vi.mocked(redis.scanKeys).mockResolvedValueOnce({
      keys: [entry('a:1')],
      cursor: '0',
      completed: true,
    })
    await store.rescan(SESSION_ID, '')
    expect(redis.cancelTask).toHaveBeenCalledWith('task-a')
    expect(session.searchStatus).toBe('idle')
    expect(session.pattern).toBe('')
    expect(keyNames(session)).toEqual(['a:1'])
  })
})

/* ── 文件夹内搜索与前缀权威（doc/frontend/key-tree.md「文件夹内搜索与前缀权威」）───────────── */

describe('前缀作用域与前缀权威', () => {
  beforeEach(async () => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
    vi.mocked(getConnection).mockResolvedValue(draft)
    vi.mocked(redis.openStoredConnection).mockResolvedValue({ ...opened, connectionId: SESSION_ID })
    vi.mocked(redis.getDatabaseList).mockResolvedValue([{ index: 0, keys: 2 }])
    vi.mocked(redis.closeConnection).mockResolvedValue(undefined)
    vi.mocked(redis.cancelTask).mockImplementation(async (taskId) => cancelledSnapshot(taskId))
    vi.mocked(redis.getTaskStatus).mockImplementation(async (taskId) => runningSnapshot(taskId))
    vi.mocked(redis.setTaskPaused).mockImplementation(async (taskId) => runningSnapshot(taskId))
    vi.mocked(redis.getKeyMeta).mockImplementation(async (_id, key) => missMeta(key))
    vi.mocked(onTaskProgress).mockImplementation(async (handler) => {
      emitTask = handler
      return () => {}
    })
    await useTasksStore().start()
  })

  /** 打开会话：初扫回一批但**未扫完** —— 前缀权威只在部分视图下才有意义。 */
  async function openPartial() {
    const store = useSessionsStore()
    vi.mocked(redis.scanKeys).mockResolvedValueOnce({
      keys: [entry('user:1')],
      cursor: '7',
      completed: false,
    })
    await store.openDb('conn-a', 0)
    return { store, session: store.sessions.get(SESSION_ID)! }
  }

  it('「在此搜索」带着当前词：前缀转义、词不转义，复用同一条任务管线', async () => {
    const { store, session } = await openPartial()
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-1' })
    await store.startSearch(SESSION_ID, 'v?')
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-2' })
    await store.searchInPrefix(SESSION_ID, 'evil[ab]')

    expect(session.searchScope).toBe('evil[ab]')
    expect(session.searchStatus).toBe('running')
    expect(vi.mocked(redis.startSearchKeys).mock.calls[1][1].pattern).toBe('evil\\[ab\\]:*v?*')
    // 旧搜索任务按代际取消 —— 就是同一条管线在跑
    expect(redis.cancelTask).toHaveBeenCalledWith('task-1')

    emitTask(taskEvent('task-2', ['evil[ab]:x']))
    await flush()
    expect(keyNames(session)).toEqual(['evil[ab]:x'])
  })

  it('作用域下词为空：pattern 就是「扫描此前缀」，仍走搜索态', async () => {
    const { store, session } = await openPartial()
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-1' })
    await store.searchInPrefix(SESSION_ID, 'user')

    expect(vi.mocked(redis.startSearchKeys).mock.calls[0][1].pattern).toBe('user:*')
    expect(session.searchStatus).toBe('running')
  })

  it('作用域下的精确探测候选是「前缀 + 分隔符 + 词」', async () => {
    const { store, session } = await openPartial()
    vi.mocked(redis.getKeyMeta).mockImplementation(async (_id, key) =>
      key === 'user:42' ? { ...missMeta(key), keyType: 'string', ttl: -1 } : missMeta(key),
    )
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-1' })
    await store.searchInPrefix(SESSION_ID, 'user')
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-2' })
    await store.startSearch(SESSION_ID, '42')
    await flush()

    expect(session.searchExact).toEqual({ key: 'user:42', keyType: 'string' })
  })

  it('清除作用域：词还在就退回全局搜索', async () => {
    const { store, session } = await openPartial()
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-1' })
    await store.startSearch(SESSION_ID, 'foo')
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-2' })
    await store.searchInPrefix(SESSION_ID, 'user')
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-3' })
    await store.clearSearchScope(SESSION_ID)

    expect(session.searchScope).toBeNull()
    expect(vi.mocked(redis.startSearchKeys).mock.calls[2][1].pattern).toBe('*foo*')
  })

  it('清除作用域：词空就回浏览态重扫', async () => {
    const { store, session } = await openPartial()
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-1' })
    await store.searchInPrefix(SESSION_ID, 'user')
    vi.mocked(redis.scanKeys).mockResolvedValueOnce({ keys: [entry('a:1')], cursor: '0', completed: true })
    await store.clearSearchScope(SESSION_ID)

    expect(session.searchScope).toBeNull()
    expect(session.searchStatus).toBe('idle')
    expect(session.pattern).toBe('')
  })

  it('前缀扫描：浏览态叠加层，按 key 名去重并入，完成后该前缀及子前缀权威', async () => {
    const { store, session } = await openPartial()
    vi.mocked(redis.startLoadAllKeys).mockResolvedValueOnce({ taskId: 'task-p' })
    await store.scanPrefix(SESSION_ID, 'user')

    expect(vi.mocked(redis.startLoadAllKeys).mock.calls[0][1].pattern).toBe('user:*')
    // 树不进搜索态 —— 这是浏览态的叠加层，不是搜索
    expect(session.searchStatus).toBe('idle')
    expect(store.isPrefixScanning(SESSION_ID, 'user')).toBe(true)

    // user:1 已被全局扫描加载过，按 key 名去重
    emitTask(taskEvent('task-p', ['user:1', 'user:2']))
    await flush()
    expect(keyNames(session)).toEqual(['user:1', 'user:2'])
    expect(store.isCountExact(SESSION_ID, 'user')).toBe(false)

    emitTask(taskEvent('task-p', [], 'completed'))
    await flush()
    expect(store.isPrefixScanning(SESSION_ID, 'user')).toBe(false)
    // 权威判定对子前缀也成立，且只认整段边界
    expect(store.isCountExact(SESSION_ID, 'user')).toBe(true)
    expect(store.isCountExact(SESSION_ID, 'user:profile')).toBe(true)
    expect(store.isCountExact(SESSION_ID, 'userx')).toBe(false)
  })

  it('前缀扫描透传任务 COUNT 设置，启动在途期的暂停广播同样补发', async () => {
    const settings = useSettingsStore()
    settings.state.taskScanCount = 4321
    const { store } = await openPartial()

    const starting = deferred<TaskStarted>()
    vi.mocked(redis.startLoadAllKeys).mockReturnValueOnce(starting.promise)
    const scanning = store.scanPrefix(SESSION_ID, 'user')
    store.setSearchTasksPaused(true)
    expect(redis.setTaskPaused).not.toHaveBeenCalled()
    starting.resolve({ taskId: 'task-p' })
    await scanning

    expect(vi.mocked(redis.startLoadAllKeys).mock.calls[0][1]).toEqual({
      pattern: 'user:*',
      count: 4321,
      withTtl: false,
    })
    expect(redis.setTaskPaused).toHaveBeenCalledWith('task-p', true)
  })

  it('窗口隐藏同步清账前缀扫描的合帧缓冲', async () => {
    const { store, session } = await openPartial()
    vi.mocked(redis.startLoadAllKeys).mockResolvedValueOnce({ taskId: 'task-p' })
    await store.scanPrefix(SESSION_ID, 'user')
    emitTask(taskEvent('task-p', ['user:2']))
    // 不等合帧宏任务，理由同搜索路径
    store.setSearchTasksPaused(true)
    expect(keyNames(session)).toEqual(['user:1', 'user:2'])
    expect(redis.setTaskPaused).toHaveBeenCalledWith('task-p', true)
  })

  it('浏览续扫与前缀叠加层去重：续扫批次里已并入的 key 不重复落账', async () => {
    const { store, session } = await openPartial()
    vi.mocked(redis.startLoadAllKeys).mockResolvedValueOnce({ taskId: 'task-p' })
    await store.scanPrefix(SESSION_ID, 'user')
    emitTask(taskEvent('task-p', ['user:2']))
    emitTask(taskEvent('task-p', [], 'completed'))
    await flush()
    expect(keyNames(session)).toEqual(['user:1', 'user:2'])
    expect(store.isCountExact(SESSION_ID, 'user')).toBe(true)

    // 「加载更多」：游标续扫的下一批必然含叠加层已并入的 user:2（SCAN 是
    // 全 keyspace 无序采样，不知道叠加层动过手）；同批内的重复（rehash）
    // 也一并挡掉。修复前这些重复条目会把权威计数撑大、挤占 treeNodeLimit
    vi.mocked(redis.scanKeys).mockResolvedValueOnce({
      keys: [entry('user:2'), entry('other:1'), entry('other:1')],
      cursor: '0',
      completed: true,
    })
    await store.scanMore(SESSION_ID)
    expect(keyNames(session)).toEqual(['user:1', 'user:2', 'other:1'])
  })

  it('前缀扫描的事件先于 invoke 响应：结果回放、权威照常落账', async () => {
    const { store, session } = await openPartial()
    const starting = deferred<TaskStarted>()
    vi.mocked(redis.startLoadAllKeys).mockReturnValueOnce(starting.promise)
    const scanning = store.scanPrefix(SESSION_ID, 'user')
    emitTask(taskEvent('task-p', ['user:2']))
    emitTask(taskEvent('task-p', [], 'completed'))
    starting.resolve({ taskId: 'task-p' })
    await scanning
    await flush()

    // 修复前：整页丢弃 + 终态无人认领 —— spinner 挂死、权威永不落账
    expect(keyNames(session)).toEqual(['user:1', 'user:2'])
    expect(store.isPrefixScanning(SESSION_ID, 'user')).toBe(false)
    expect(store.isCountExact(SESSION_ID, 'user')).toBe(true)
  })

  it('作用域态（词空）下刷新：重启前缀搜索任务，不退回单批扫描', async () => {
    const { store, session } = await openPartial()
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-1' })
    await store.searchInPrefix(SESSION_ID, 'user')
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-2' })
    await store.rescan(SESSION_ID)

    expect(redis.cancelTask).toHaveBeenCalledWith('task-1')
    expect(vi.mocked(redis.startSearchKeys).mock.calls[1][1].pattern).toBe('user:*')
    expect(session.searchStatus).toBe('running')
    // chip 保留：刷新是重跑当前视图，不是清空搜索
    expect(session.searchScope).toBe('user')
    expect(redis.scanKeys).toHaveBeenCalledTimes(1)
  })

  it('rescan 作废权威集', async () => {
    const { store } = await openPartial()
    vi.mocked(redis.startLoadAllKeys).mockResolvedValueOnce({ taskId: 'task-p' })
    await store.scanPrefix(SESSION_ID, 'user')
    emitTask(taskEvent('task-p', [], 'completed'))
    await flush()
    expect(store.isCountExact(SESSION_ID, 'user')).toBe(true)

    vi.mocked(redis.scanKeys).mockResolvedValueOnce({ keys: [], cursor: '9', completed: false })
    await store.rescan(SESSION_ID)
    expect(store.isCountExact(SESSION_ID, 'user')).toBe(false)
  })

  it('前缀扫描被取消：进行中标记清干净、不置权威，已并入的结果保留', async () => {
    const { store, session } = await openPartial()
    vi.mocked(redis.startLoadAllKeys).mockResolvedValueOnce({ taskId: 'task-p' })
    await store.scanPrefix(SESSION_ID, 'user')
    emitTask(taskEvent('task-p', ['user:9'], 'cancelled'))
    await flush()

    expect(keyNames(session)).toEqual(['user:1', 'user:9'])
    expect(store.isPrefixScanning(SESSION_ID, 'user')).toBe(false)
    expect(store.isCountExact(SESSION_ID, 'user')).toBe(false)
  })

  it('搜索态下前缀扫描的守卫直接弹回 —— 界面入口也已禁用', async () => {
    const { store } = await openPartial()
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-s' })
    await store.startSearch(SESSION_ID, 'foo')
    await store.scanPrefix(SESSION_ID, 'user')

    expect(redis.startLoadAllKeys).not.toHaveBeenCalled()
  })

  it('进搜索态拆掉在途的前缀扫描 —— 两条管线不许同时灌一个会话', async () => {
    const { store } = await openPartial()
    vi.mocked(redis.startLoadAllKeys).mockResolvedValueOnce({ taskId: 'task-p' })
    await store.scanPrefix(SESSION_ID, 'user')
    vi.mocked(redis.startSearchKeys).mockResolvedValueOnce({ taskId: 'task-s' })
    await store.startSearch(SESSION_ID, 'foo')

    expect(redis.cancelTask).toHaveBeenCalledWith('task-p')
    expect(store.isPrefixScanning(SESSION_ID, 'user')).toBe(false)
  })

  it('前缀扫描触到树节点上限：取消任务、不置权威（与搜索上限同一条规则）', async () => {
    const settings = useSettingsStore()
    settings.state.treeNodeLimit = 3
    const { store, session } = await openPartial()
    vi.mocked(redis.startLoadAllKeys).mockResolvedValueOnce({ taskId: 'task-p' })
    await store.scanPrefix(SESSION_ID, 'user')

    emitTask(taskEvent('task-p', ['user:2', 'user:3', 'user:4', 'user:5']))
    await flush()
    expect(session.keys).toHaveLength(3)
    expect(session.truncated).toBe(true)
    expect(redis.cancelTask).toHaveBeenCalledWith('task-p')
    expect(store.isPrefixScanning(SESSION_ID, 'user')).toBe(false)
    expect(store.isCountExact(SESSION_ID, 'user')).toBe(false)
  })
})

describe('能力的运行时降级（applyCapabilityChange）', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.resetAllMocks()
    vi.mocked(getConnection).mockResolvedValue(draft)
    vi.mocked(redis.openStoredConnection).mockResolvedValue({ ...opened, connectionId: SESSION_ID })
    vi.mocked(redis.getDatabaseList).mockResolvedValue([{ index: 0, keys: 2 }])
    vi.mocked(redis.closeConnection).mockResolvedValue(undefined)
  })

  it('只改事件点名的那一项，其余保持建连时的判定', async () => {
    const { store, session, firstScan, opening } = await openWithHangingScan()
    firstScan.resolve({ keys: [], cursor: '0', completed: true })
    await opening

    const degraded: Capability = {
      state: 'unsupported',
      usable: false,
      reason: '保存时保留过期时间（KEEPTTL）被服务端拒绝（命令或参数不被识别），本会话内按不支持处理',
    }
    store.applyCapabilityChange({ connectionId: SESSION_ID, id: 'setKeepTtl', capability: degraded })

    expect(session.capabilities.setKeepTtl).toEqual(degraded)
    expect(session.capabilities.scripting.usable).toBe(true)
  })

  it('会话已经关掉时丢弃事件 —— 事件可能晚于 closeSession 到达', () => {
    const store = useSessionsStore()
    expect(() =>
      store.applyCapabilityChange({
        connectionId: 'gone',
        id: 'scripting',
        capability: { state: 'unsupported', usable: false, reason: '' },
      }),
    ).not.toThrow()
  })
})
