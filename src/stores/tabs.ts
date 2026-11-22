import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { useSessionsStore } from './sessions'

/**
 * 主区标签页。
 *
 * 除 key 外的五种标签都由标签栏的 `+` 下拉新建，见
 * doc/frontend/tabs.md「入口：标签栏的 `+` 菜单」。标题必须带作用域，
 * 因为同时开 prod-east/db0 与 staging/db3 两个 Key 浏览器是常见需求。
 */
export type TabKind = 'key' | 'browser' | 'cli' | 'monitor' | 'pubsub' | 'memory'

export interface Tab {
  id: string
  kind: TabKind
  sessionId: string
  /** key 标签页专用 */
  key?: string
  keyType?: string | null
  /** Key 浏览器 / 内存分析打开时带的初始 MATCH */
  pattern?: string
  /** 从状态栏内存读数进来时：直接开分析模式并按内存降序 */
  sortByMemory?: boolean
  /**
   * 预览页（VS Code 的 preview tab）。全局只有一个，标题斜体，
   * 再单击别的 key 就地换内容而不是新开一个 —— 光是逛一遍 keyspace
   * 就攒下二十个标签页，然后一个个关，这是最烦人的一段体验。
   */
  preview?: boolean
}

const KIND_LABEL: Record<TabKind, string> = {
  key: '',
  browser: 'Key 浏览器',
  cli: 'CLI',
  monitor: 'Monitor',
  pubsub: '订阅',
  memory: '内存分析',
}

let counter = 0
function nextId(kind: TabKind) {
  counter += 1
  return `${kind}-${counter}`
}

export const useTabsStore = defineStore('tabs', () => {
  const sessions = useSessionsStore()
  const tabs = ref<Tab[]>([])
  const activeId = ref<string | null>(null)

  const active = computed(() => tabs.value.find((tab) => tab.id === activeId.value) ?? null)

  /** `prod-east / db0`。找不到会话时给个占位，不要让标题变成裸 id。 */
  function scopeLabel(sessionId: string): string {
    const session = sessions.sessions.get(sessionId)
    if (!session) return '已断开'
    return session.cluster ? session.name : `${session.name} / db${session.db}`
  }

  /** 同时开着多个实例时才需要连接名 —— 只开一个时 `db0` 已经无歧义，也省下宽度。 */
  const multiConnection = computed(() => {
    const keys = new Set<string>()
    for (const session of sessions.sessions.values()) keys.add(session.connectionKey ?? session.id)
    return keys.size > 1
  })

  function shortScope(sessionId: string): string {
    const session = sessions.sessions.get(sessionId)
    if (!session) return '已断开'
    if (session.cluster) return session.name
    return multiConnection.value ? `${session.name} / db${session.db}` : `db${session.db}`
  }

  function kindLabel(tab: Tab): string {
    return KIND_LABEL[tab.kind]
  }

  function activate(id: string) {
    activeId.value = id

    /* 作用域必须跟着当前标签页走。
       这不是「侧栏随动定位」那个功能（那个不做）—— 这里只改作用域，
       不滚动、不展开、不动树。不改的话，从 prod/db0 的 key 切到
       staging/db3 的 CLI 之后，状态栏、搜索框、`+` 菜单的作用域还停在 db0，
       下一个新标签页就会开到错误的库上去。 */
    const tab = tabs.value.find((item) => item.id === id)
    if (!tab || !sessions.sessions.has(tab.sessionId)) return
    sessions.activeSessionId = tab.sessionId
    // 树里的选中项如果属于别的会话，它会盖过 activeSessionId 决定作用域，清掉
    if (sessions.selected && sessions.selected.sessionId !== tab.sessionId) {
      sessions.selected = null
    }
  }

  /**
   * 打开标签页。
   *
   * key 标签页有三条规则，顺序不能反：
   *   1. 同一个 (会话, key) 已经开着 → 切过去；如果这次是驻留打开，顺手转正
   *   2. 这次是预览打开且已有预览页 → **就地换内容**，不新建
   *   3. 其余情况才真的新建
   *
   * 非 key 标签页一律驻留：它们是你主动要的，不是浏览产物。
   */
  function open(tab: Omit<Tab, 'id'>): Tab {
    if (tab.kind === 'key') {
      const existing = tabs.value.find(
        (item) => item.kind === 'key' && item.sessionId === tab.sessionId && item.key === tab.key,
      )
      if (existing) {
        if (!tab.preview) existing.preview = false
        activate(existing.id)
        return existing
      }

      if (tab.preview) {
        const index = tabs.value.findIndex((item) => item.preview)
        if (index >= 0) {
          // 整体替换而不是 Object.assign：后者不会清掉上一个 key 留下的字段。
          // id 保持不变，标签在栏里的位置因此也不会跳。
          const replaced: Tab = { ...tab, id: tabs.value[index].id }
          tabs.value.splice(index, 1, replaced)
          activate(replaced.id)
          return replaced
        }
      }
    }

    const created: Tab = { ...tab, id: nextId(tab.kind) }
    tabs.value.push(created)
    activate(created.id)
    return created
  }

  /** 把预览页转正。双击标签、双击树里的 key、或对该 key 写入成功后都会调它。 */
  function promote(id: string) {
    const tab = tabs.value.find((item) => item.id === id)
    if (tab) tab.preview = false
  }

  function close(id: string) {
    const index = tabs.value.findIndex((tab) => tab.id === id)
    if (index < 0) return
    tabs.value.splice(index, 1)
    if (activeId.value !== id) return
    // 关掉当前页后落到右邻居，没有右邻居再落到左邻居 —— 与浏览器一致
    const next = tabs.value[index] ?? tabs.value[index - 1] ?? null
    activeId.value = next?.id ?? null
  }

  /** 会话关闭时它的标签页全部失效，留在界面上只会点出一堆连接不存在的错误。 */
  function closeBySession(sessionId: string) {
    for (const tab of tabs.value.filter((item) => item.sessionId === sessionId)) close(tab.id)
  }

  function closeOthers(id: string) {
    for (const tab of tabs.value.filter((item) => item.id !== id)) close(tab.id)
  }

  function closeRight(id: string) {
    const index = tabs.value.findIndex((tab) => tab.id === id)
    if (index < 0) return
    for (const tab of tabs.value.slice(index + 1)) close(tab.id)
  }

  function closeAll() {
    for (const tab of [...tabs.value]) close(tab.id)
  }

  /** key 被删除或过期后，对应标签页要么关掉要么标记为已失效。 */
  function findKeyTab(sessionId: string, key: string): Tab | undefined {
    return tabs.value.find(
      (tab) => tab.kind === 'key' && tab.sessionId === sessionId && tab.key === key,
    )
  }

  return {
    tabs,
    activeId,
    active,
    scopeLabel,
    shortScope,
    kindLabel,
    open,
    promote,
    close,
    closeOthers,
    closeRight,
    closeAll,
    closeBySession,
    activate,
    findKeyTab,
  }
})
