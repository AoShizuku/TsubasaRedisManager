import { onBeforeUnmount, ref, shallowRef, triggerRef } from 'vue'
import { cancelTask } from '@/api/redis'
import { useTasksStore } from '@/stores/tasks'
import { useUiStore } from '@/stores/ui'
import type { TaskEventItem, TaskStarted } from '@/types/ipc'

/**
 * 消费任务事件流（Monitor / 订阅 / 内存分析共用）。
 *
 * task-progress 是唯一的高频频道：Monitor 一秒可能上千条。
 * 两条纪律：
 *   1. 条目进 shallowRef 的普通数组，逐帧 flush —— 每条都触发响应式更新，
 *      滚动会直接卡死；
 *   2. 环形上限，超出丢最旧的 —— 挂一夜的 Monitor 否则会把内存吃光。
 */
export function useTaskStream<T>(options: {
  limit?: number
  map: (item: TaskEventItem) => T | null
}) {
  const tasks = useTasksStore()
  const ui = useUiStore()
  const limit = options.limit ?? 5000

  const rows = shallowRef<T[]>([])
  const running = ref(false)
  const taskId = ref<string | null>(null)
  const paused = ref(false)
  /** 因为超出上限被丢掉的条数，界面上要说出来，不能装作没发生 */
  const dropped = ref(0)

  let pending: T[] = []
  let frame = 0
  let unsubscribe: (() => void) | null = null

  function flush() {
    frame = 0
    if (!pending.length) return
    const next = rows.value.concat(pending)
    pending = []
    if (next.length > limit) {
      dropped.value += next.length - limit
      next.splice(0, next.length - limit)
    }
    rows.value = next
    triggerRef(rows)
  }

  function schedule() {
    if (frame) return
    frame = requestAnimationFrame(flush)
  }

  async function start(starter: () => Promise<TaskStarted>) {
    if (running.value) return
    try {
      const started = await starter()
      taskId.value = started.taskId
      running.value = true
      dropped.value = 0
      unsubscribe = tasks.subscribe(started.taskId, (items) => {
        if (paused.value) return
        for (const item of items) {
          const mapped = options.map(item)
          if (mapped !== null) pending.push(mapped)
        }
        schedule()
      })
    } catch (error) {
      ui.notifyError(error, '启动任务失败')
    }
  }

  async function stop() {
    if (!taskId.value) return
    try {
      await cancelTask(taskId.value)
    } catch {
      // 任务可能已经自己结束了
    }
    running.value = false
    unsubscribe?.()
    unsubscribe = null
    taskId.value = null
  }

  function clear() {
    pending = []
    dropped.value = 0
    rows.value = []
    triggerRef(rows)
  }

  onBeforeUnmount(() => {
    if (frame) cancelAnimationFrame(frame)
    unsubscribe?.()
    // 标签页关掉了就别让后端继续推
    if (taskId.value) void cancelTask(taskId.value).catch(() => undefined)
  })

  return { rows, running, paused, dropped, taskId, start, stop, clear }
}
