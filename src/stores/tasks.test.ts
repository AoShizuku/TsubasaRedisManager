import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import * as redis from '@/api/redis'
import { onTaskProgress } from '@/api/events'
import type { TaskProgressEvent, TaskStatus, TaskStatusSnapshot } from '@/types/ipc'
import { useTasksStore } from './tasks'

/*
 * 任务总线的两个防泄漏兜底都围绕「事件流指望不上」的窗口：
 * watchSettled 的后端状态查询、无主缓冲的 TTL 过期。
 * 与 sessions.test.ts 同一套做法：mock 掉 @/api 层，事件由测试端直接投递。
 */
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

vi.mock('@/api/redis', () => ({
  cancelTask: vi.fn(),
  getTaskStatus: vi.fn(),
}))

vi.mock('@/api/events', () => ({
  onTaskProgress: vi.fn(),
}))

/* 终态清理定时器走 window.setTimeout，node 环境里补一个最小桩 */
vi.stubGlobal('window', {
  setTimeout: globalThis.setTimeout.bind(globalThis),
  clearTimeout: globalThis.clearTimeout.bind(globalThis),
})

let emitTask: (event: TaskProgressEvent) => void = () => {}

function taskEvent(taskId: string, status: TaskStatus, keys: string[] = []): TaskProgressEvent {
  return {
    taskId,
    connectionId: 'sess-1',
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

function snapshot(taskId: string, status: TaskStatus): TaskStatusSnapshot {
  return {
    taskId,
    connectionId: 'sess-1',
    connectionName: 'test',
    taskType: 'search_keys',
    status,
    processed: 0,
    succeeded: 0,
    failed: 0,
    total: null,
    estimatedProgress: null,
    message: null,
  }
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

/** 让在途 promise 的 then/catch 链走完（微任务不够，隔一个宏任务最稳）。 */
async function flush() {
  await new Promise<void>((resolve) => setTimeout(resolve, 0))
}

beforeEach(async () => {
  setActivePinia(createPinia())
  vi.resetAllMocks()
  vi.mocked(onTaskProgress).mockImplementation(async (handler) => {
    emitTask = handler
    return () => {}
  })
  await useTasksStore().start()
})

describe('watchSettled 的兜底查询（终态与摘要都被清理后的迟到注册）', () => {
  it('map 未命中且后端已终态：兜底查询兑现 waiter，不再永远挂起', async () => {
    vi.mocked(redis.getTaskStatus).mockResolvedValueOnce(snapshot('task-a', 'completed'))
    const store = useTasksStore()

    // 修复前：事件与摘要都被 4 秒清理删光（invoke 响应晚于清理窗口的
    // 病理性延迟），这里挂一个永不 resolve 的 waiter
    await expect(store.watchSettled('task-a')).resolves.toMatchObject({
      taskId: 'task-a',
      status: 'completed',
    })
  })

  it('后端还在跑：兜底不抢答，终态仍由事件兑现', async () => {
    vi.mocked(redis.getTaskStatus).mockResolvedValueOnce(snapshot('task-a', 'progress'))
    const store = useTasksStore()
    const settled = store.watchSettled('task-a')
    await flush()

    emitTask(taskEvent('task-a', 'completed'))
    await expect(settled).resolves.toMatchObject({ status: 'completed' })
  })

  it('事件与兜底竞争：先到的事件兑现，晚到的兜底不双发', async () => {
    const probe = deferred<TaskStatusSnapshot>()
    vi.mocked(redis.getTaskStatus).mockReturnValueOnce(probe.promise)
    const store = useTasksStore()
    const settled = store.watchSettled('task-a')

    // 兜底还在途，终态事件先到 —— waiter 由事件兑现并从表里摘掉
    emitTask(taskEvent('task-a', 'cancelled'))
    probe.resolve(snapshot('task-a', 'completed'))
    await flush()

    // 结果是事件的 cancelled；兜底晚到时 waiter 表已空，不会再兑现一遍
    await expect(settled).resolves.toMatchObject({ status: 'cancelled' })
    expect(redis.getTaskStatus).toHaveBeenCalledTimes(1)
  })

  it('后端也查不到（这个 id 从未注册过）：以 failed 收场而不是挂死', async () => {
    vi.mocked(redis.getTaskStatus).mockRejectedValueOnce(new Error('task not found'))
    const store = useTasksStore()

    // failed 而不是 completed：结果真伪不可知，标成功会让消费方把
    // 不完整的结果当成权威（诚实底线）；挂死则正是这条兜底要修的病
    await expect(store.watchSettled('task-ghost')).resolves.toMatchObject({
      taskId: 'task-ghost',
      status: 'failed',
    })
  })

  it('同一任务的并发 watchSettled 共用一次兜底查询', async () => {
    vi.mocked(redis.getTaskStatus).mockResolvedValue(snapshot('task-a', 'completed'))
    const store = useTasksStore()
    const first = store.watchSettled('task-a')
    const second = store.watchSettled('task-a')

    await expect(first).resolves.toMatchObject({ status: 'completed' })
    await expect(second).resolves.toMatchObject({ status: 'completed' })
    expect(redis.getTaskStatus).toHaveBeenCalledTimes(1)
  })
})

describe('无主事件缓冲的 TTL 过期（终态事件彻底丢失时的常驻兜底）', () => {
  it('断流超过 TTL 的无主缓冲被后续事件的清扫回收，新缓冲不受影响', async () => {
    const store = useTasksStore()
    const now = vi.spyOn(Date, 'now')
    now.mockReturnValue(1_000_000)
    // task-old 的事件无人认领进缓冲，此后终态丢失、再无事件
    emitTask(taskEvent('task-old', 'progress', ['a:1']))

    // 5 分钟后别的任务来了一条事件，顺带触发清扫
    now.mockReturnValue(1_000_000 + 5 * 60_000)
    emitTask(taskEvent('task-new', 'progress', ['b:1']))

    // 修复前：task-old 的缓冲只能等终态清理定时器连带删除，而终态永远
    // 不来 —— 每任务封顶 64 批的条目常驻到进程退出
    const oldSink = vi.fn()
    store.subscribe('task-old', oldSink)
    expect(oldSink).not.toHaveBeenCalled()

    const newSink = vi.fn()
    store.subscribe('task-new', newSink)
    expect(newSink).toHaveBeenCalledTimes(1)
    now.mockRestore()
  })

  it('TTL 之内的无主缓冲照常回放', async () => {
    const store = useTasksStore()
    const now = vi.spyOn(Date, 'now')
    now.mockReturnValue(1_000_000)
    emitTask(taskEvent('task-a', 'progress', ['a:1']))

    // 差一毫秒到期：清扫不动它，subscribe 回放照旧
    now.mockReturnValue(1_000_000 + 5 * 60_000 - 1)
    emitTask(taskEvent('task-b', 'progress', ['b:1']))
    const sink = vi.fn()
    store.subscribe('task-a', sink)
    expect(sink).toHaveBeenCalledTimes(1)
    now.mockRestore()
  })
})
