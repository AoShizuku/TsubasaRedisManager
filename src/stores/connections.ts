import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import * as api from '@/api/connections'
import { toFault, type AppFault } from '@/api/invoke'
import type { ConnectionSaveDraft, ConnectionSummary } from '@/types/ipc'

const RECENT_KEY = 'tsubasa.recent.v1'
const RECENT_LIMIT = 6

interface RecentEntry {
  key: string
  at: number
}

function loadRecent(): RecentEntry[] {
  try {
    const raw = localStorage.getItem(RECENT_KEY)
    return raw ? (JSON.parse(raw) as RecentEntry[]) : []
  } catch {
    return []
  }
}

/** 存储态连接列表。秘密在系统密钥链里，这里只有 has* 标志。 */
export const useConnectionsStore = defineStore('connections', () => {
  const items = ref<ConnectionSummary[]>([])
  const loading = ref(false)
  const error = ref<AppFault | null>(null)
  const recent = ref<RecentEntry[]>(loadRecent())

  const byKey = computed(() => {
    const map = new Map<string, ConnectionSummary>()
    for (const item of items.value) map.set(item.key, item)
    return map
  })

  /** 最近打开：只用来填首屏空状态，找不到对应连接的条目直接丢掉。 */
  const recentConnections = computed(() =>
    recent.value
      .map((entry) => ({ at: entry.at, connection: byKey.value.get(entry.key) }))
      .filter((entry): entry is { at: number; connection: ConnectionSummary } => !!entry.connection),
  )

  async function refresh() {
    loading.value = true
    error.value = null
    try {
      items.value = await api.listConnections()
    } catch (e) {
      error.value = toFault(e)
    } finally {
      loading.value = false
    }
  }

  async function save(draft: ConnectionSaveDraft): Promise<ConnectionSummary> {
    const saved = await api.saveConnection(draft)
    await refresh()
    return saved
  }

  async function remove(key: string) {
    await api.deleteConnection(key)
    recent.value = recent.value.filter((entry) => entry.key !== key)
    persistRecent()
    await refresh()
  }

  async function reorder(keys: string[]) {
    items.value = await api.reorderConnections(keys)
  }

  async function migrateLegacy(payload: unknown) {
    const result = await api.migrateLegacyConnections(payload)
    await refresh()
    return result
  }

  function persistRecent() {
    localStorage.setItem(RECENT_KEY, JSON.stringify(recent.value))
  }

  function markUsed(key: string) {
    recent.value = [{ key, at: Date.now() }, ...recent.value.filter((e) => e.key !== key)].slice(
      0,
      RECENT_LIMIT,
    )
    persistRecent()
  }

  return {
    items,
    loading,
    error,
    byKey,
    recentConnections,
    refresh,
    save,
    remove,
    reorder,
    migrateLegacy,
    markUsed,
  }
})
