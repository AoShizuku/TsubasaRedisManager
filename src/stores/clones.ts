import { defineStore } from 'pinia'
import { reactive } from 'vue'
import { getCloneTask } from '@/api/redis'
import { useSessionsStore } from './sessions'
import { useTabsStore } from './tabs'
import { useTasksStore } from './tasks'
import { useUiStore } from './ui'
import type { CloneTaskSnapshot } from '@/types/ipc'

export const cloneRunning = (phase: CloneTaskSnapshot['phase']) => ['queued', 'connecting', 'reading', 'restoring'].includes(phase)
const order = { queued: 0, connecting: 1, reading: 2, restoring: 3, completed: 4, cancelled: 4, failed: 4, uncertain: 4 }

/** 独立于对话框生命周期：关闭 modal 后仍消费完成事件、刷新目标并保留结果。 */
export const useClonesStore = defineStore('clones', () => {
  const jobs = reactive(new Map<string, CloneTaskSnapshot>())
  const tracking = new Set<string>()
  const tasks = useTasksStore()
  const sessions = useSessionsStore()
  const tabs = useTabsStore()
  const ui = useUiStore()

  function track(taskId: string, openAfter: boolean) {
    if (tracking.has(taskId) || (jobs.has(taskId) && !cloneRunning(jobs.get(taskId)!.phase))) return
    tracking.add(taskId)
    let stopped = false
    let unsubscribe = () => {}
    let timer: ReturnType<typeof setTimeout> | undefined
    function finish(detail: CloneTaskSnapshot) {
      stopped = true
      unsubscribe()
      clearTimeout(timer)
      tracking.delete(taskId)
      setTimeout(() => tasks.tasks.delete(taskId), 4000)
      if (detail.phase === 'completed') {
        const id = detail.targetConnectionId
        const session = sessions.sessions.get(id)
        if (session) {
          sessions.invalidateKeyspace(id)
          if (session.searchQuery || session.searchScope !== null || session.pattern) void sessions.rescan(id)
          else sessions.rememberKey(id, detail.targetKey, detail.keyType)
          if (session.connectionKey) void sessions.loadDbList(session.connectionKey, id)
          if (openAfter) tabs.open({ kind: 'key', sessionId: id, key: detail.targetKey, keyType: detail.keyType, preview: false })
        }
        ui.notify('ok', `已克隆至 ${detail.targetName}`, detail.targetKey)
      } else if (detail.phase === 'cancelled') {
        ui.notify('info', '已取消克隆，目标未写入')
      } else {
        ui.notify('danger', `克隆至 ${detail.targetName}：${detail.message}`, detail.targetKey)
      }
      // 只保留最近 20 个终态记录，进行中的任务不裁剪。
      const settled = [...jobs.values()].filter(job => !cloneRunning(job.phase))
      for (const job of settled.slice(0, -20)) jobs.delete(job.taskId)
    }
    function accept(next: CloneTaskSnapshot) {
      if (stopped) return
      const previous = jobs.get(taskId)
      if (previous && order[next.phase] < order[previous.phase]) return
      jobs.set(taskId, { ...next, bytes: next.bytes ?? previous?.bytes ?? null })
      // 事件丢失时，轮询也要同步状态栏的总线摘要，不能留下“永远运行中”。
      tasks.tasks.set(taskId, {
        taskId, connectionId: next.targetConnectionId, connectionName: next.targetName, taskType: 'clone_key',
        status: next.phase === 'completed' ? 'completed' : next.phase === 'cancelled' ? 'cancelled' : next.phase === 'failed' || next.phase === 'uncertain' ? 'failed' : 'progress',
        processed: cloneRunning(next.phase) || next.phase === 'cancelled' ? 0 : 1,
        succeeded: next.phase === 'completed' ? 1 : 0,
        failed: next.phase === 'failed' || next.phase === 'uncertain' ? 1 : 0,
        total: 1, estimatedProgress: null, message: next.message,
      })
      if (!cloneRunning(next.phase)) finish(next)
    }
    unsubscribe = tasks.subscribe(taskId, items => {
      for (const item of items) if (item.type === 'clone') accept(item.snapshot)
    })
    if (stopped) { unsubscribe(); return }
    // 事件可能丢失或先于 invoke 到达。低频查询兜底，查询失败不臆断服务端失败。
    async function poll() {
      if (stopped) return
      try { accept(await getCloneTask(taskId)) } catch { /* 下次恢复后继续核对。 */ }
      if (!stopped) timer = setTimeout(() => void poll(), 1000)
    }
    void poll()
  }
  return { jobs, track }
})
