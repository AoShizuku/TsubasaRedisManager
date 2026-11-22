import { ref, shallowRef } from 'vue'
import { toFault, type AppFault } from '@/api/invoke'

const PAGE = 200

export interface IndexPage<T> {
  items: T[]
  total: number
}

/**
 * 下标分页（List / ZSet）。
 *
 * 和 SCAN 家族不同，下标分页是**有序**的 —— List 的插入顺序、ZSet 的分数序
 * 都靠它保住。SCAN 给不了顺序，所以这两种类型的主视图走这里。
 *
 * v0.1 这里还得靠 `stride` 参数和 `Array.isArray(reply[0])` 去猜 ZRANGE
 * 在 RESP2/RESP3 下是扁平数组还是成对数组 —— 猜错就会在半页处以为读完了。
 * 现在后端直接给 `total`，边界是算出来的而不是猜出来的。
 */
export function useRangeCollection<T>(fetchPage: (start: number, stop: number) => Promise<IndexPage<T>>) {
  const items = shallowRef<T[]>([])
  const offset = ref(0)
  const total = ref(0)
  const complete = ref(false)
  const loading = ref(false)
  const error = ref<AppFault | null>(null)

  async function loadMore() {
    if (loading.value || complete.value) return
    loading.value = true
    error.value = null
    try {
      const start = offset.value
      const page = await fetchPage(start, start + PAGE - 1)
      items.value = items.value.concat(page.items)
      offset.value = start + page.items.length
      total.value = page.total
      // 空页也算到底：区间超出长度时 Redis 回空数组，不会报错
      complete.value = page.items.length === 0 || offset.value >= page.total
    } catch (e) {
      error.value = toFault(e)
      // 出错就停在这里，否则会在同一段下标上反复撞墙
      complete.value = true
    } finally {
      loading.value = false
    }
  }

  async function reset() {
    items.value = []
    offset.value = 0
    complete.value = false
    await loadMore()
  }

  return { items, offset, total, complete, loading, error, loadMore, reset }
}
