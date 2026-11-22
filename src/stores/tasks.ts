import { defineStore } from 'pinia'
import { computed, reactive, ref } from 'vue'
import { onTaskProgress, type UnlistenFn } from '@/api/events'
import { cancelTask, getTaskStatus } from '@/api/redis'
import type { TaskEventItem, TaskProgressEvent, TaskStatus } from '@/types/ipc'

/**
 * 任务进度总线。
 *
 * task-progress 是唯一的高频频道 —— Monitor 一秒可能上千条。
 * 所以这里只维护"进度摘要"这一份响应式状态，条目本身通过订阅回调直接
 * 交给消费方，不进 store：把上千条 monitor 记录塞进 reactive 数组
 * 会让每一条都触发依赖收集。
 */
export interface TaskSummary {
  taskId: string
  connectionId: string
  connectionName: string
  taskType: string
  status: TaskProgressEvent['status']
  processed: number
  succeeded: number
  failed: number
  total: number | null
  /** 扫描类任务的近似进度（0..1，null = 不可用）。显示必须带「约」。 */
  estimatedProgress: number | null
  message: string | null
}

type ItemSink = (items: TaskEventItem[], event: TaskProgressEvent) => void

/**
 * 无主事件每任务最多缓冲这么多批，超限丢最早的。上限只是防泄漏的兜底：
 * 缓冲覆盖的是「任务启动的 invoke 还在途、首批事件已到达」这几十毫秒的
 * 窗口（本机小 keyspace 一页扫完时必现），正常几批就被 subscribe 领走。
 */
const PENDING_EVENT_LIMIT = 64

/**
 * 无主缓冲的常驻兜底。正常出路有两条：subscribe 回放、终态的 4 秒清理。
 * 终态事件本身丢失且无人 subscribe 时两条都走不到，条目会以每任务 64 批
 * 的体量常驻 —— 按最后一次写入过期。清扫挂在后续 handle() 调用上，
 * 不为它开定时器（保持条目不进响应式的同一条原则：这层是纯数据搬运）。
 */
const PENDING_EVENT_TTL_MS = 5 * 60_000

interface PendingEvents {
  /** 最后一次写入的时刻。任务还活着就会不断刷新，只有断流的缓冲会过期。 */
  at: number
  events: TaskProgressEvent[]
}

function isSettledStatus(status: TaskStatus): boolean {
  return status !== 'started' && status !== 'progress'
}

export const useTasksStore = defineStore('tasks', () => {
  const tasks = reactive(new Map<string, TaskSummary>())
  const sinks = new Map<string, ItemSink>()
  /**
   * 无主 taskId 的事件缓冲：事件可能赶在发起任务的 invoke 响应之前到达，
   * 那时 sink 还没注册，直接丢会把小库搜索的整页结果（乃至全部结果）丢光。
   * subscribe 时回放并清掉。**刻意不进 reactive**（同头部注释的原则 ——
   * 条目不进响应式 store），普通 Map + 数组即可；终态清理定时器一并删除，
   * 防没人认领的缓冲滞留。
   */
  const pendingEvents = new Map<string, PendingEvents>()
  /** 等待任务进终态的人。见 watchSettled。 */
  const settledWaiters = new Map<string, Array<(summary: TaskSummary) => void>>()
  /** 正在跑的兜底状态查询（见 settleFromBackend），防同一任务重复发。 */
  const settleProbes = new Set<string>()
  const unlisten = ref<UnlistenFn | null>(null)

  const running = computed(() =>
    Array.from(tasks.values()).filter(
      (task) => task.status === 'started' || task.status === 'progress',
    ),
  )

  const runningCount = computed(() => running.value.length)

  function sweepPendingEvents(now: number) {
    for (const [taskId, pending] of pendingEvents) {
      if (now - pending.at >= PENDING_EVENT_TTL_MS) pendingEvents.delete(taskId)
    }
  }

  function handle(event: TaskProgressEvent) {
    sweepPendingEvents(Date.now())
    const summary: TaskSummary = {
      taskId: event.taskId,
      connectionId: event.connectionId,
      connectionName: event.connectionName,
      taskType: event.taskType,
      status: event.status,
      processed: event.processed,
      succeeded: event.succeeded,
      failed: event.failed,
      total: event.total,
      estimatedProgress: event.estimatedProgress,
      message: event.message,
    }
    tasks.set(event.taskId, summary)

    // 带估算的空页事件也要送达消费方：MATCH 稀疏时扫描任务整页可能一个
    // 不中，只有估算在前进 —— 不转发，搜索态的「约 P%」就停在上一页
    if (event.items.length || event.estimatedProgress !== null) {
      const sink = sinks.get(event.taskId)
      if (sink) {
        sink(event.items, event)
      } else {
        // 消费方还没注册（发起任务的 invoke 响应在途）：缓冲住等 subscribe
        // 回放。只缓冲本会转发的事件 —— sink 从来收不到别的
        let pending = pendingEvents.get(event.taskId)
        if (!pending) {
          pending = { at: 0, events: [] }
          pendingEvents.set(event.taskId, pending)
        }
        pending.at = Date.now()
        pending.events.push(event)
        if (pending.events.length > PENDING_EVENT_LIMIT) pending.events.shift()
      }
    }

    // 终态任务保留一小会儿供状态栏显示结果，然后自行退场。
    // watchSettled 的迟到注册靠 tasks 里这份终态摘要即时兑现（见下）。
    if (isSettledStatus(event.status)) {
      settledWaiters.get(event.taskId)?.forEach((resolve) => resolve(summary))
      settledWaiters.delete(event.taskId)
      window.setTimeout(() => {
        tasks.delete(event.taskId)
        sinks.delete(event.taskId)
        pendingEvents.delete(event.taskId)
      }, 4000)
    }
  }

  async function start() {
    if (unlisten.value) return
    unlisten.value = await onTaskProgress(handle)
  }

  async function stop() {
    unlisten.value?.()
    unlisten.value = null
  }

  /**
   * 消费方在拿到 taskId 后立刻注册；返回取消函数。
   * 「立刻」也可能太晚 —— 事件先于 invoke 响应到达时已进无主缓冲，
   * 注册的同时同步回放给 sink，然后缓冲退场。
   */
  function subscribe(taskId: string, sink: ItemSink): () => void {
    sinks.set(taskId, sink)
    const pending = pendingEvents.get(taskId)
    if (pending) {
      pendingEvents.delete(taskId)
      for (const event of pending.events) sink(event.items, event)
    }
    return () => {
      if (sinks.get(taskId) === sink) sinks.delete(taskId)
    }
  }

  /**
   * 等一个任务走到终态，拿它的最终计数。
   *
   * 不能用 subscribe：那个 sink 只在事件带 items 时才被调用，而批量删除 /
   * 批量 TTL 这类任务只报计数、不发条目。
   *
   * 返回 promise 而不是回调注册在组件上，是因为**发起任务的对话框会立刻关闭**
   * （不该让一个 modal 挂在那里几分钟），组件早就卸载了，结果还是要报出来。
   *
   * 任务可能在注册前就已进终态（小库一页扫完，completed 赶在 invoke 响应
   * 之前）：终态事件到达即消费掉 waiters，之后注册的没人再叫醒 ——
   * 所以先查 tasks 里的摘要，已终态就立即兑现，别挂一个永不 resolve 的等待。
   *
   * map 里连条目都没有时还有最后一格：终态事件与摘要都已被 4 秒清理
   * 定时器删掉（invoke 响应比清理窗口还晚的病理性延迟）。这时事件流
   * 不会再叫醒任何人，兜底向后端的任务注册表查一次（settleFromBackend）。
   */
  function watchSettled(taskId: string): Promise<TaskSummary> {
    const existing = tasks.get(taskId)
    if (existing && isSettledStatus(existing.status)) {
      return Promise.resolve(existing)
    }
    return new Promise((resolve) => {
      const waiters = settledWaiters.get(taskId) ?? []
      waiters.push(resolve)
      settledWaiters.set(taskId, waiters)
      if (!existing) void settleFromBackend(taskId)
    })
  }

  /**
   * watchSettled 的兜底：事件流指望不上时问一次后端。三种收场 ——
   * 后端已终态：拿快照兑现；还在跑：不抢答，终态照常由事件兑现；
   * 查不到（后端的注册表终态后也不清记录，走到这一格说明这个 id 后端
   * 从来就没认过）：以 failed 兑现。**failed 而不是挂死或 completed**：
   * 挂死正是本兜底要修的病；completed 会让搜索把不完整的结果标成权威
   * （scanComplete 摘 `+`），撒谎；failed 让消费方如实呈现「结果不可信」。
   */
  async function settleFromBackend(taskId: string) {
    if (settleProbes.has(taskId)) return
    settleProbes.add(taskId)
    try {
      let summary: TaskSummary
      try {
        const snapshot = await getTaskStatus(taskId)
        if (!isSettledStatus(snapshot.status)) return
        summary = { ...snapshot }
      } catch {
        summary = {
          taskId,
          connectionId: '',
          connectionName: '',
          taskType: '',
          status: 'failed',
          processed: 0,
          succeeded: 0,
          failed: 0,
          total: null,
          estimatedProgress: null,
          message: '任务状态已不可查',
        }
      }
      // 与事件路径竞争的裁判是 settledWaiters 条目本身：handle() 兑现时
      // 会删掉它，这里拿不到就说明已经有人 resolve 过 —— 不双发
      const waiters = settledWaiters.get(taskId)
      if (!waiters) return
      settledWaiters.delete(taskId)
      waiters.forEach((resolve) => resolve(summary))
    } finally {
      settleProbes.delete(taskId)
    }
  }

  async function cancel(taskId: string) {
    const snapshot = await cancelTask(taskId)
    const existing = tasks.get(taskId)
    if (existing) existing.status = snapshot.status
  }

  return { tasks, running, runningCount, start, stop, subscribe, watchSettled, cancel }
})
