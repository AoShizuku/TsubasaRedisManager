import { ref } from 'vue'
import { getKeyMeta } from '@/api/redis'
import { toFault, type AppFault } from '@/api/invoke'
import type { HllMeta } from '@/types/ipc'

/**
 * Key 的元信息。
 *
 * 一次 `redis_get_key_meta` 拿回全部字段。v0.1 这里串行发 TYPE / TTL / <size> /
 * OBJECT ENCODING / MEMORY USAGE / CLUSTER KEYSLOT 六条命令，每打开一个标签页
 * 跑一遍；协议知识与并发编排都在渲染进程里。见 doc/architecture.md「前后端分层」。
 *
 * memory / encoding / slot 为空是**正常情况**而不是错误：命令可能被 ACL 或
 * rename-command 挡掉、槽位只有集群才有。后端已经用 Option 表达了这一点，
 * 这里照实显示为空即可。
 */
export function useKeyMeta(sessionId: string, key: string) {
  const type = ref<string | null>(null)
  const ttl = ref<number | null>(null)
  const size = ref<number | null>(null)
  const encoding = ref<string | null>(null)
  const memory = ref<number | null>(null)
  const slot = ref<number | null>(null)
  /** 非空 = HLL 子形态（string 且带合法 HYLL 头），KeyTab 据此换查看器 */
  const hll = ref<HllMeta | null>(null)
  /** TYPE 返回 none 或 TTL 返回 -2：key 已经不在了，不要继续显示一张陈旧的表 */
  const missing = ref(false)
  const loading = ref(false)
  const error = ref<AppFault | null>(null)

  async function refresh() {
    loading.value = true
    error.value = null
    try {
      const meta = await getKeyMeta(sessionId, key)
      type.value = meta.keyType
      ttl.value = meta.ttl
      size.value = meta.size
      encoding.value = meta.encoding
      memory.value = meta.memory
      slot.value = meta.slot
      hll.value = meta.hll
      missing.value = meta.keyType === null || meta.ttl === -2
    } catch (e) {
      error.value = toFault(e)
    } finally {
      loading.value = false
    }
  }

  return { type, ttl, size, encoding, memory, slot, hll, missing, loading, error, refresh }
}
