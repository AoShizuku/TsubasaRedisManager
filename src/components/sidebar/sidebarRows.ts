import { computed, type ComputedRef } from 'vue'
import { flattenTree, type TreeRow } from '@/lib/keyTree'
import { useConnectionsStore } from '@/stores/connections'
import { useSessionsStore } from '@/stores/sessions'
import { useSettingsStore } from '@/stores/settings'
import type { ConnectionSummary } from '@/types/ipc'

/**
 * 侧栏是一棵**复合**树：`连接 → DB → namespace → key` 在同一个虚拟列表里。
 * 前两级来自 store，后面来自 flattenTree。这里把三者拼成一维行序列，
 * 渲染层只管画，不用关心行是从哪来的。
 *
 * depth 是**合成后的**深度：连接 0、DB 1、key 树的第一层 2。
 * 渲染时直接拿它当 --ind，不要再减 —— 减掉会让第一层命名空间的缩进
 * 比它的父 DB 行还浅，"缩进即层级"这条唯一的视觉线索就断了。
 */
export type SidebarRow =
  | {
      kind: 'conn'
      id: string
      depth: number
      connection: ConnectionSummary
      expanded: boolean
      opened: boolean
    }
  | {
      kind: 'db'
      id: string
      depth: number
      connectionKey: string
      db: number
      keys: number | null
      expanded: boolean
      sessionId: string | null
    }
  | { kind: 'node'; id: string; depth: number; sessionId: string; row: TreeRow }
  | {
      kind: 'more'
      id: string
      depth: number
      sessionId: string
      loading: boolean
      loaded: number
      truncated: boolean
      /**
       * 浏览态的分母：DB 的 DBSIZE（`dbLists`，可能为 null）。
       * 「已加载 N / M」是浏览态最诚实的一句话（doc/frontend/key-tree.md「诚实信号」）。
       * 搜索态恒为 null —— MATCH 下已加载数数的是命中数，除以全库大小没有意义。
       */
      total: number | null
    }
  /**
   * 搜索态的置顶行：精确命中（doc/frontend/key-tree.md「精确命中」）。
   * 与流式结果按 key 名去重 —— 树里不会再出现同名的第二行。
   */
  | {
      kind: 'exact'
      id: string
      depth: number
      sessionId: string
      key: string
      keyType: string | null
    }
  /**
   * 搜索态的底部行：任务状态替代「加载更多」（doc/frontend/key-tree.md「搜索态」「诚实信号」）。
   * 搜索没有「更多」可点 —— 任务自己会扫到底。found 含精确命中那一行。
   */
  | {
      kind: 'search'
      id: string
      depth: number
      sessionId: string
      status: 'running' | 'limited' | 'cancelled' | 'failed'
      found: number
      /** 「已达上限（N）」的 N：settings.treeNodeLimit */
      limit: number
      /**
       * 近似进度（0..1，null = 不可用 → 保持不定进度）。只在 running 态给，
       * 显示必须带「约」（doc/frontend/key-tree.md「诚实信号」的硬要求）。
       */
      progress: number | null
    }
  | { kind: 'hint'; id: string; depth: number; text: string }

export function useSidebarRows(): ComputedRef<SidebarRow[]> {
  const connections = useConnectionsStore()
  const sessions = useSessionsStore()
  const settings = useSettingsStore()

  return computed<SidebarRow[]>(() => {
    const rows: SidebarRow[] = []

    for (const connection of connections.items) {
      const expanded = sessions.expandedConnections.has(connection.key)
      const dbs = sessions.dbLists.get(connection.key)
      rows.push({
        kind: 'conn',
        id: `conn:${connection.key}`,
        depth: 0,
        connection,
        expanded,
        opened: !!dbs,
      })
      if (!expanded) continue

      if (!dbs) {
        rows.push({ kind: 'hint', id: `hint:${connection.key}`, depth: 1, text: '正在连接…' })
        continue
      }

      for (const db of dbs) {
        const sessionId = sessions.sessionIdFor(connection.key, db.index)
        const dbExpanded = sessions.isDbExpanded(connection.key, db.index)
        rows.push({
          kind: 'db',
          id: `db:${connection.key}/${db.index}`,
          depth: 1,
          connectionKey: connection.key,
          db: db.index,
          keys: db.keys,
          expanded: dbExpanded,
          sessionId,
        })
        if (!dbExpanded || !sessionId) continue

        const session = sessions.sessions.get(sessionId)
        const tree = sessions.treeOf(sessionId)
        if (!session || !tree) continue

        const searching = session.searchStatus !== 'idle'
        const exact = searching ? session.searchExact : null
        if (exact) {
          rows.push({
            kind: 'exact',
            id: `exact:${sessionId}`,
            depth: 2,
            sessionId,
            key: exact.key,
            keyType: exact.keyType,
          })
        }

        // 列表模式下这棵树本身就是用空分隔符建的（见 sessions store 的
        // separatorFor），所以这里只需关掉紧凑合并，不需要另一套渲染路径。
        const flat = settings.state.defaultTreeMode === 'list'
        const treeRows = flattenTree(tree, {
          expanded: sessions.pathsOf(sessionId),
          compact: settings.state.compactPaths && !flat,
          separator: flat ? '' : session.separator,
        })

        for (const row of treeRows) {
          rows.push({
            kind: 'node',
            id: `${sessionId}:${row.id}`,
            depth: 2 + row.depth,
            sessionId,
            row,
          })
        }

        // 「没有匹配的 Key」只在扫完时说（搜索态即任务 completed —— 那时
        // scanComplete 才被置位）；有精确命中就不是零结果，这句话不出现
        if (treeRows.length === 0 && session.scanComplete && !exact) {
          rows.push({
            kind: 'hint',
            id: `empty:${sessionId}`,
            depth: 2,
            text: session.pattern ? '没有匹配的 Key' : '这个数据库是空的',
          })
        }

        // 「加载更多」是浏览态的行：搜索态任务自动扫到底，没有「更多」可点，
        // 底部行换成任务状态（doc/frontend/key-tree.md「搜索态」）
        if (!searching && !session.scanComplete) {
          rows.push({
            kind: 'more',
            id: `more:${sessionId}`,
            depth: 2,
            sessionId,
            loading: session.scanning,
            loaded: session.keys.length,
            truncated: session.truncated,
            total: session.pattern ? null : db.keys,
          })
        }

        if (
          session.searchStatus === 'running' ||
          session.searchStatus === 'limited' ||
          session.searchStatus === 'cancelled' ||
          session.searchStatus === 'failed'
        ) {
          rows.push({
            kind: 'search',
            id: `search:${sessionId}`,
            depth: 2,
            sessionId,
            status: session.searchStatus,
            found: session.keys.length + (exact ? 1 : 0),
            limit: settings.state.treeNodeLimit,
            progress: session.searchStatus === 'running' ? session.searchProgress : null,
          })
        }
      }
    }

    return rows
  })
}
