import { ref, shallowRef } from 'vue'
import { toFault, type AppFault } from '@/api/invoke'
import { useSettingsStore } from '@/stores/settings'

export interface CursorPage<T> {
  items: T[]
  cursor: string
  completed: boolean
}

/**
 * 游标分页（Hash / Set）。
 *
 * 调用方给一个取一页的函数，这里只管游标状态与累积。命令怎么发、
 * RESP2 与 RESP3 的形状差异怎么吃掉，全在后端 —— 见
 * doc/backend/ipc-contract.md「参数形状按分页语义」。
 *
 * SCAN 家族的 COUNT 是**提示**不是配额：一次可能回 0 条，
 * 游标没回到 0 就说明还没完，不能拿"这次没数据"当结束条件。
 */
export function useScanCollection<T>(
  fetchPage: (cursor: string, pattern: string, count: number) => Promise<CursorPage<T>>,
) {
  const settings = useSettingsStore()

  /* shallowRef：几万个成员逐个做响应式代理毫无必要。
     代价是必须整体替换数组，不能 push。 */
  const items = shallowRef<T[]>([])
  const cursor = ref('0')
  const complete = ref(false)
  const loading = ref(false)
  const pattern = ref('')
  const error = ref<AppFault | null>(null)

  async function loadMore() {
    if (loading.value || complete.value) return
    loading.value = true
    error.value = null
    try {
      const page = await fetchPage(cursor.value, pattern.value, settings.state.scanCount)
      items.value = items.value.concat(page.items)
      cursor.value = page.cursor
      complete.value = page.completed
    } catch (e) {
      error.value = toFault(e)
      // 出错就别继续翻了，否则会在同一个游标上反复撞墙
      complete.value = true
    } finally {
      loading.value = false
    }
  }

  async function reset(nextPattern?: string) {
    if (nextPattern !== undefined) pattern.value = nextPattern
    items.value = []
    cursor.value = '0'
    complete.value = false
    await loadMore()
  }

  return { items, cursor, complete, loading, pattern, error, loadMore, reset }
}
