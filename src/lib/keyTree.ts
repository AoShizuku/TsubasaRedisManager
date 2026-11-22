import type { KeyEntry } from '@/types/ipc'

/**
 * Key 树的数据层。
 *
 * 紧凑路径（单链合并）在这里算完，渲染层只拿到一维的 TreeRow 数组。
 * 见 doc/frontend/key-tree.md「紧凑路径」。
 */

export interface KeyNode {
  /** 完整前缀（namespace 不含尾分隔符）或完整 key。折叠态、选中态一律用它做标识。 */
  path: string
  /** 本层的段名 */
  name: string
  isKey: boolean
  keyType: string | null
  /** 只在扫描时要了 TTL 才有；-1 = 永不过期 */
  ttl: number | null
  children: Map<string, KeyNode>
  /** 已加载的子孙 key 数。SCAN 未完成时这是部分值。 */
  keyCount: number
}

function makeNode(path: string, name: string): KeyNode {
  return { path, name, isKey: false, keyType: null, ttl: null, children: new Map(), keyCount: 0 }
}

/**
 * 按分隔符折叠成树。
 *
 * 分隔符为空时不折叠，每个 key 直接挂在根下 —— 有些库根本不用命名空间约定，
 * 强行按单字符切会切出一堆无意义的层级。
 */
export function buildKeyTree(entries: readonly KeyEntry[], separator: string): KeyNode {
  const root = makeNode('', '')

  for (const entry of entries) {
    const segments = separator ? entry.key.split(separator) : [entry.key]
    let node = root
    let prefix = ''

    for (let i = 0; i < segments.length; i += 1) {
      const segment = segments[i]
      prefix = i === 0 ? segment : `${prefix}${separator}${segment}`
      const isLast = i === segments.length - 1

      let child = node.children.get(segment)
      if (!child) {
        child = makeNode(prefix, segment)
        node.children.set(segment, child)
      }
      // 计数落在祖先上，节点自己不统计自己
      node.keyCount += 1

      if (isLast) {
        child.isKey = true
        child.keyType = entry.keyType
        child.ttl = entry.ttl
      }
      node = child
    }
  }

  return root
}

export interface TreeSegment {
  text: string
  /** 该段对应的完整前缀。右键这一段时用它拼 MATCH：`cache:page:*` */
  path: string
}

export type TreeRowKind = 'ns' | 'key'

export interface TreeRow {
  /** 虚拟列表的稳定 key，等于该行最深节点的完整路径 */
  id: string
  path: string
  depth: number
  kind: TreeRowKind
  keyType: string | null
  /** 只在扫描时要了 TTL 才有；-1 = 永不过期 */
  ttl: number | null
  /** 紧凑合并后这一行显示的所有段。未合并时只有一段。 */
  segments: TreeSegment[]
  hasChildren: boolean
  expanded: boolean
  keyCount: number
  /**
   * 被合并掉的中间节点路径（不含最深那个）。
   * 展开这一行时必须把它们一起标记为展开，否则等 SCAN 又送来新 key
   * 让合并行裂开时，已经展开的子树会突然藏起来。
   */
  mergedPaths: string[]
}

export interface FlattenOptions {
  expanded: ReadonlySet<string>
  /** 紧凑路径开关，默认开启 */
  compact: boolean
  separator: string
}

/** 文件夹在前、key 在后，各自按自然序。数字段按数值比较，避免 1002 排在 999 前面。 */
const collator = new Intl.Collator('en', { numeric: true, sensitivity: 'base' })

function sortChildren(children: Map<string, KeyNode>): KeyNode[] {
  const list = Array.from(children.values())
  list.sort((a, b) => {
    const aFolder = a.children.size > 0
    const bFolder = b.children.size > 0
    if (aFolder !== bFolder) return aFolder ? -1 : 1
    return collator.compare(a.name, b.name)
  })
  return list
}

/**
 * 沿单子节点链一路合并，直到遇到第一个有多个子节点的层级。
 * 末端是单个 key 也一并并进来 —— `lock:job:88213` 这种"命名空间下只有一个 key"
 * 的形态在 Redis 里极其常见，为它单独占一行加一级缩进是纯亏损。
 *
 * 自身既是 key 又有子节点的节点不参与合并：它得保留自己的身份。
 */
function collapseChain(start: KeyNode, separator: string) {
  const segments: TreeSegment[] = [{ text: start.name, path: start.path }]
  const mergedPaths: string[] = []
  let node = start

  while (separator && !node.isKey && node.children.size === 1) {
    const only = node.children.values().next().value as KeyNode
    mergedPaths.push(node.path)
    segments.push({ text: only.name, path: only.path })
    node = only
  }

  return { node, segments, mergedPaths }
}

/** 把树摊平成可渲染的行序列。只输出展开可见的行。 */
export function flattenTree(root: KeyNode, options: FlattenOptions): TreeRow[] {
  const rows: TreeRow[] = []

  const walk = (parent: KeyNode, depth: number) => {
    for (const child of sortChildren(parent.children)) {
      const collapsed = options.compact
        ? collapseChain(child, options.separator)
        : { node: child, segments: [{ text: child.name, path: child.path }], mergedPaths: [] }

      const node = collapsed.node
      const hasChildren = node.children.size > 0
      const expanded = hasChildren && options.expanded.has(node.path)

      rows.push({
        id: node.path,
        path: node.path,
        depth,
        // 一个节点可以既是 key 又是前缀（`user` 与 `user:1` 同时存在）。
        // 这种行同时给出类型色点和展开箭头：点名字打开 key，点箭头展开子树。
        kind: node.isKey ? 'key' : 'ns',
        keyType: node.keyType,
        ttl: node.ttl,
        segments: collapsed.segments,
        hasChildren,
        expanded,
        keyCount: node.keyCount,
        mergedPaths: collapsed.mergedPaths,
      })

      if (expanded) walk(node, depth + 1)
    }
  }

  walk(root, 0)
  return rows
}

/** 展开一行：连同被合并掉的中间层一起标记，见 TreeRow.mergedPaths 的说明。 */
export function expandRow(expanded: Set<string>, row: TreeRow): void {
  expanded.add(row.path)
  for (const path of row.mergedPaths) expanded.add(path)
}

export function collapseRow(expanded: Set<string>, row: TreeRow): void {
  expanded.delete(row.path)
}

/**
 * 转义 Redis glob 的元字符。
 *
 * `stringmatchlen` 认 `* ? [ ] \`，反斜杠后的字符按字面比较。不转义的话，
 * key 名里的 `[` 会让整个前缀变成另一个意思：树里的 `evil[ab]` 段拼出
 * `evil[ab]:*`，`[ab]` 成了字符类 —— **匹配不到 `evil[ab]:1` 本身，
 * 却会命中 `evila:1`**。在 Key 浏览器入口下这只是列表不对；
 * 批量删除接上同一条路之后，它就是删掉一个无关的 key。
 *
 * 转义的边界是**「从前缀生成模式」这一步**。对话框里用户手输的模式
 * 不能转义 —— 那里他要的就是 glob。
 */
function escapeGlob(text: string): string {
  return text.replace(/[\\*?\[\]]/g, (char) => `\\${char}`)
}

/** 从某个前缀拼出 SCAN 的 MATCH 模式。空前缀表示整个 DB。 */
export function matchPatternFor(prefix: string, separator: string): string {
  if (!prefix) return '*'
  return `${escapeGlob(prefix)}${escapeGlob(separator)}*`
}

/**
 * 前缀作用域下的搜索模式（doc/frontend/key-tree.md「文件夹内搜索与前缀权威」）：
 * `escapeGlob(prefix + sep)` + 用户词。
 *
 * 前缀来自树，是字面路径，必须转义；用户词**不转义** —— 与全局搜索同一条
 * 规则（`*` 包裹规则也一致），否则同一个词在两个搜索入口里语义分裂。
 * 词为空时退化成 `matchPatternFor`，即「扫描此前缀」。
 */
export function scopedSearchPattern(prefix: string, separator: string, query: string): string {
  const word = query.trim()
  if (!word) return matchPatternFor(prefix, separator)
  const wrapped = word.includes('*') ? word : `*${word}*`
  if (!prefix) return wrapped
  return `${escapeGlob(prefix)}${escapeGlob(separator)}${wrapped}`
}

/**
 * 前缀权威集的包含关系判定（doc/frontend/key-tree.md「前缀权威」）：`path` 本身在集合里，
 * 或它的某个祖先前缀在集合里 —— `user` 权威则 `user:profile` 也权威，
 * 它是子树的子树。集合按**完整前缀**存，绝不是行索引：紧凑合并行裂开后
 * 行索引会错位（doc/frontend/key-tree.md「增量到达会改变合并结果」）。
 *
 * 逐级剥段查 Set 而不是遍历集合做 startsWith：查找是 O(段数)，与权威集
 * 大小无关；且天然只认整段边界 —— `user` 权威不会覆盖 `userx:1`。
 */
export function isPrefixCovered(
  prefixes: ReadonlySet<string>,
  path: string,
  separator: string,
): boolean {
  if (prefixes.size === 0 || !path) return false
  if (prefixes.has(path)) return true
  if (!separator) return false
  let end = path.length
  while ((end = path.lastIndexOf(separator, end - 1)) > 0) {
    if (prefixes.has(path.slice(0, end))) return true
  }
  return false
}

/** 展开到指定 key 所需的全部祖先前缀，用于"在树中定位"。 */
export function ancestorPaths(key: string, separator: string): string[] {
  if (!separator) return []
  const segments = key.split(separator)
  const paths: string[] = []
  let prefix = ''
  for (let i = 0; i < segments.length - 1; i += 1) {
    prefix = i === 0 ? segments[i] : `${prefix}${separator}${segments[i]}`
    paths.push(prefix)
  }
  return paths
}
