import { reactive, ref } from 'vue'
import { getKeyMeta } from '@/api/redis'

export interface KeyAnalysis {
  type: string | null
  ttl: number | null
  memory: number | null
  encoding: string | null
}

const CONCURRENCY = 8

/**
 * Key 浏览器的分析数据。
 *
 * 每个 key 一次 `redis_get_key_meta`（后端两批 pipeline）。v0.1 是每 key
 * 四条串行命令 —— TYPE / TTL / OBJECT ENCODING / MEMORY USAGE，
 * 而且要自己判断 MEMORY 是不是被只读守卫挡掉了。
 *
 * 成本仍然必须显式化，所以这依旧是一个带成本提示的开关而不是免费默认项：
 * 一次 IPC 不等于零开销，服务端那边还是每 key 五条命令。
 *
 * 而且这里**不能只取可见行** —— 排序需要全部已加载 key 的数据，
 * 只取可见行会排出一个随滚动位置变化的假顺序。这也正是底部那句
 * "排序只作用于已加载部分"必须写出来的原因。
 */
export function useKeyAnalysis(sessionId: string) {
  const data = reactive(new Map<string, KeyAnalysis>())
  const running = ref(false)
  const done = ref(0)
  const total = ref(0)

  let cancelled = false

  async function run(keys: readonly string[]) {
    const todo = keys.filter((key) => !data.has(key))
    if (!todo.length) return

    cancelled = false
    running.value = true
    total.value = todo.length
    done.value = 0

    const queue = [...todo]
    const worker = async () => {
      while (queue.length && !cancelled) {
        const key = queue.shift()
        if (key === undefined) break
        try {
          const meta = await getKeyMeta(sessionId, key)
          data.set(key, {
            type: meta.keyType,
            ttl: meta.ttl,
            // 拿不到内存/编码是正常情况（ACL、rename-command），后端已用 null 表达
            memory: meta.memory,
            encoding: meta.encoding,
          })
        } catch {
          data.set(key, { type: null, ttl: null, memory: null, encoding: null })
        } finally {
          done.value += 1
        }
      }
    }

    await Promise.all(Array.from({ length: Math.min(CONCURRENCY, queue.length) }, () => worker()))
    running.value = false
  }

  function cancel() {
    cancelled = true
    running.value = false
  }

  function clear() {
    cancel()
    data.clear()
    done.value = 0
    total.value = 0
  }

  return { data, running, done, total, run, cancel, clear }
}
