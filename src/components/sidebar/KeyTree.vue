<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { useVirtualizer } from '@tanstack/vue-virtual'
import Icon from '@/components/ui/Icon.vue'
import TypeDot from '@/components/ui/TypeDot.vue'
import CtxMenu from '@/components/ui/CtxMenu.vue'
import MenuItem from '@/components/ui/MenuItem.vue'
import { collapseRow, expandRow, matchPatternFor } from '@/lib/keyTree'
import { formatApproxPercent, formatCount, formatPartialCount, formatTtl } from '@/lib/format'
import { useConnectionsStore } from '@/stores/connections'
import { useSessionsStore } from '@/stores/sessions'
import { useSettingsStore } from '@/stores/settings'
import { useTabsStore } from '@/stores/tabs'
import { useUiStore } from '@/stores/ui'
import { useSidebarRows, type SidebarRow } from './sidebarRows'

const connections = useConnectionsStore()
const sessions = useSessionsStore()
const settings = useSettingsStore()
const tabs = useTabsStore()
const ui = useUiStore()

const emit = defineEmits<{ deleteKey: [sessionId: string, key: string] }>()

async function createInDb(row: Extract<SidebarRow, { kind: 'db' }>) {
  try {
    const id = row.sessionId ?? await sessions.openDb(row.connectionKey, row.db)
    ui.openCreateKey(id)
  } catch (error) { ui.notifyError(error, '打开数据库失败') }
}

function createInPrefix(sessionId: string, prefix: string) {
  const separator = sessions.sessions.get(sessionId)?.separator ?? ':'
  ui.openCreateKey(sessionId, prefix + separator)
}

const rows = useSidebarRows()
const scroller = ref<HTMLElement | null>(null)
const cursor = ref(-1)

/* 行高来自 tokens.css 的 --h-tree（紧凑 26 / 标准 30 / 宽松 34）。
   虚拟化必须在 JS 里知道它，改 tokens 时这里要一起改。 */
const ROW_HEIGHT: Record<string, number> = { compact: 26, standard: 30, cozy: 34 }
const rowHeight = computed(() => ROW_HEIGHT[settings.state.density] ?? 30)

const virtualizer = useVirtualizer(
  computed(() => ({
    count: rows.value.length,
    getScrollElement: () => scroller.value,
    estimateSize: (index: number) => {
      const kind = rows.value[index]?.kind
      return kind === 'more' || kind === 'search' ? 26 : rowHeight.value
    },
    overscan: 12,
    getItemKey: (index: number) => rows.value[index]?.id ?? index,
  })),
)

const totalSize = computed(() => virtualizer.value.getTotalSize())

/** 把虚拟条目和行数据配好对再进模板：这样 v-if 才能对联合类型正常收窄。 */
const visible = computed(() =>
  virtualizer.value
    .getVirtualItems()
    .map((item) => ({ item, row: rows.value[item.index] }))
    .filter((entry): entry is { item: (typeof entry)['item']; row: SidebarRow } => !!entry.row),
)

/* 类型色点直接来自 SCAN —— 后端在同一次往返里 pipeline 了一批 TYPE。
   v0.1 这里有一个"给可见行补类型"的 watch，滚到哪取到哪，
   色点会在滚动后慢慢浮现；现在数据一到就是全的。 */
function typeFor(row: Extract<SidebarRow, { kind: 'node' }>) {
  return row.row.keyType
}

/* TTL 只在设置项打开时才随 SCAN 一起取回来，没开时一律为 null。
   永不过期（-1）不显示 —— 那是绝大多数 key 的状态，显示出来只是噪声。 */
function ttlOf(row: Extract<SidebarRow, { kind: 'node' }>): number | null {
  if (!settings.state.treeTtl || row.row.kind !== 'key') return null
  const ttl = row.row.ttl
  return ttl === null || ttl < 0 ? null : ttl
}

function isTtlSoon(row: Extract<SidebarRow, { kind: 'node' }>): boolean {
  const ttl = ttlOf(row)
  return ttl !== null && ttl <= settings.state.ttlSoonThreshold
}

function rowStyle(item: { start: number; size: number }, indent: number) {
  return {
    '--ind': indent,
    height: `${item.size}px`,
    transform: `translateY(${item.start}px)`,
  }
}

/** 右键落在哪一段上。整行只挂一个菜单，段落信息在捕获阶段先记下来。 */
const ctxSegment = ref<{ path: string; text: string } | null>(null)

function captureSegment(event: MouseEvent, row: Extract<SidebarRow, { kind: 'node' }>) {
  const target = (event.target as HTMLElement | null)?.closest<HTMLElement>('[data-seg-path]')
  const segments = row.row.segments
  const last = segments[segments.length - 1]
  ctxSegment.value = {
    path: target?.dataset.segPath ?? last.path,
    text: target?.dataset.segText ?? last.text,
  }
}

/* ── 交互 ─────────────────────────────────────────────── */

function toggleNode(row: Extract<SidebarRow, { kind: 'node' }>) {
  if (!row.row.hasChildren) return
  const expanded = sessions.pathsOf(row.sessionId)
  if (row.row.expanded) collapseRow(expanded, row.row)
  else expandRow(expanded, row.row)
}

/**
 * 单击 = 预览页，双击 / 右键「打开」= 驻留页。
 * 双击时第一次点击已经开出预览页了，dblclick 再把同一个标签转正 ——
 * 不需要延时去区分单双击。
 */
function openKey(sessionId: string, key: string, keyType: string | null, preview = true) {
  sessions.selected = { sessionId, path: key }
  sessions.activeSessionId = sessionId
  tabs.open({ kind: 'key', sessionId, key, keyType, preview })
}

/** 双击 key 行：把刚才单击开出来的预览页转正。 */
function onRowDblClick(row: SidebarRow) {
  if (row.kind !== 'node' || row.row.kind !== 'key') return
  openKey(row.sessionId, row.row.path, typeFor(row), false)
}

async function activate(row: SidebarRow) {
  switch (row.kind) {
    case 'conn':
      if (row.expanded) {
        sessions.expandedConnections.delete(row.connection.key)
      } else {
        try {
          await sessions.openConnection(row.connection.key)
        } catch (error) {
          ui.notifyError(error, `连接 ${row.connection.name} 失败`)
        }
      }
      break
    case 'db':
      if (row.sessionId && row.expanded) {
        sessions.toggleDb(row.connectionKey, row.db)
      } else {
        try {
          const id = await sessions.openDb(row.connectionKey, row.db)
          sessions.expandedDbs.add(`${row.connectionKey}/${row.db}`)
          sessions.activeSessionId = id
        } catch (error) {
          ui.notifyError(error, `打开 db${row.db} 失败`)
        }
      }
      break
    case 'node':
      sessions.selected = { sessionId: row.sessionId, path: row.row.path }
      sessions.activeSessionId = row.sessionId
      // 一个节点可以既是 key 又是前缀（`user` 与 `user:1` 同时存在）。
      // 这种行点名字打开 key，展开交给箭头槽 —— 不能因为它有子节点
      // 就退化成折叠，那样这个 key 就永远打不开了。
      if (row.row.kind === 'key') {
        openKey(row.sessionId, row.row.path, typeFor(row))
      } else {
        toggleNode(row)
      }
      break
    case 'more':
      await sessions.scanMore(row.sessionId)
      break
    case 'exact':
      // 置顶行与树里的 key 行同一套打开逻辑
      openKey(row.sessionId, row.key, row.keyType)
      break
    default:
      break
  }
}

/**
 * 右键 namespace 段 → 带着前缀进 Key 浏览器。
 * 树负责定位，表格负责分析，两者用同一个前缀串起来。
 */
function openBrowserFor(sessionId: string, prefix: string) {
  const session = sessions.sessions.get(sessionId)
  tabs.open({
    kind: 'browser',
    sessionId,
    pattern: matchPatternFor(prefix, session?.separator ?? ':'),
  })
}

/**
 * 右键 namespace → 批量设置 TTL。
 *
 * 作用范围是**模式**而不是一堆勾 —— 树不做多选：一个紧凑合并行代表好几段路径，
 * 勾它是什么意思？而且多选态还要和展开/折叠、虚拟滚动共存。
 * 分段命中（右键 `cache` 段拿 `cache:*`）表达的正是"一个范围"，
 * 比多选更贴合树的语义。见 doc/frontend/batch.md「两个入口」。
 */
function openBatchTtlFor(sessionId: string, prefix: string) {
  const session = sessions.sessions.get(sessionId)
  ui.openBatchTtl({
    sessionId,
    keys: [],
    pattern: matchPatternFor(prefix, session?.separator ?? ':'),
    scope: 'pattern',
  })
}

/**
 * 右键 namespace → 批量删除。作用范围同样是**模式**，理由与批量 TTL 一致
 * （树不做多选，见上面那段与 doc/frontend/batch.md「两个入口」）。
 *
 * 多传一个 `prefix`：删完之后树里那一片必须当场消失。树不跟着 keyspace
 * 版本号刷新 —— 它是显式加载的快照 —— 所以什么都不做的话，删完 `cache:page:*`
 * 树上原封不动，点进去每个 key 都报不存在，看起来就是坏了。
 */
function openBatchDeleteFor(sessionId: string, prefix: string) {
  // 空前缀在别处是良性的默认值（`matchPatternFor('')` = `*` = 整个 DB，
  // 「在 Key 浏览器中打开」拿它只是看一眼），在这里是清空这个 DB，
  // 而且 `forgetPrefix('')` 会把整棵树的已加载 key 一起裁掉。
  // 今天 ctxSegment 走不到空串，但不可撤销的操作值得让它不可表达。
  if (!prefix) return
  const session = sessions.sessions.get(sessionId)
  const separator = session?.separator ?? ':'
  ui.openBatchDelete({
    sessionId,
    keys: [],
    pattern: matchPatternFor(prefix, separator),
    scope: 'pattern',
    // 裁树要的是转义前的字面前缀，与 pattern 不是一回事
    prefix: `${prefix}${separator}`,
  })
}

function isReadonly(sessionId: string) {
  return !!sessions.sessions.get(sessionId)?.readonly
}

/** 关闭一个 DB 会话：连带关掉它的标签页，否则会留下一堆点不动的面板。 */
async function closeDb(sessionId: string | null) {
  if (!sessionId) return
  tabs.closeBySession(sessionId)
  await sessions.closeSession(sessionId)
}

async function disconnect(connectionKey: string) {
  for (const session of Array.from(sessions.sessions.values())) {
    if (session.connectionKey === connectionKey) tabs.closeBySession(session.id)
  }
  await sessions.closeConnection(connectionKey)
}

async function removeConnection(connectionKey: string, name: string) {
  const ok = await ui.confirm({
    title: '删除连接',
    body: '连接配置与保存在系统密钥链里的密码都会被删除。服务端的数据不受影响。',
    detail: name,
    confirmText: '删除',
    danger: true,
  })
  if (!ok) return
  try {
    await disconnect(connectionKey)
    await connections.remove(connectionKey)
    ui.notify('ok', '连接已删除')
  } catch (error) {
    ui.notifyError(error, '删除连接失败')
  }
}

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    ui.notify('ok', '已复制')
  } catch (error) {
    ui.notifyError(error, '复制失败')
  }
}

function separatorOf(sessionId: string) {
  return sessions.sessions.get(sessionId)?.separator ?? ':'
}

/**
 * 计数是否精确（可以摘 `+`）：全库扫完，或该前缀被权威集覆盖
 * （doc/frontend/key-tree.md「前缀权威」）。包含关系（`user` 权威 ⇒ `user:profile`
 * 也权威）在 store 里判。
 */
function countExact(row: Extract<SidebarRow, { kind: 'node' }>): boolean {
  return sessions.isCountExact(row.sessionId, row.row.path)
}

/** 该行是否有前缀正在扫描（计数位换 spinner）。紧凑合并行看每一段：
    右键中间段发起的扫描也要在这一行上有反馈。 */
function prefixBusy(row: Extract<SidebarRow, { kind: 'node' }>): boolean {
  return row.row.segments.some((segment) => sessions.isPrefixScanning(row.sessionId, segment.path))
}

/** `N+` 的 tooltip：可点时多说一句发现路径（doc/frontend/key-tree.md「诚实信号」）；不可点（搜索态 / 已到
    树上限）就只陈述事实，别许诺一个按不动的动作。 */
function countTitle(row: Extract<SidebarRow, { kind: 'node' }>): string | undefined {
  if (countExact(row)) return undefined
  return sessions.canScanPrefix(row.sessionId, row.row.path)
    ? '扫描未完成，实际数量可能更多。点击扫描此前缀。'
    : '扫描未完成，实际数量可能更多'
}

/** 点 `N+` 徽标 = 扫描此前缀。只在可扫时拦下点击 ——
    精确计数上的点击继续冒泡成整行的展开/折叠，不吞掉。 */
function onCountClick(event: MouseEvent, row: Extract<SidebarRow, { kind: 'node' }>) {
  if (!sessions.canScanPrefix(row.sessionId, row.row.path)) return
  event.stopPropagation()
  void sessions.scanPrefix(row.sessionId, row.row.path)
}

/** 「扫描此前缀」禁用时说明为什么，与只读禁用项同一套 title 提示风格。 */
function scanPrefixHint(sessionId: string, prefix: string | null | undefined): string | undefined {
  if (!prefix) return undefined
  const session = sessions.sessions.get(sessionId)
  if (session && session.searchStatus !== 'idle') return '搜索进行中，先退出搜索'
  if (sessions.isPrefixScanning(sessionId, prefix)) return '该前缀正在扫描'
  if (sessions.isCountExact(sessionId, prefix)) return '该前缀已扫描完整'
  return undefined
}

/** 右键「在此搜索」：作用域 chip 与搜索框都在侧栏顶部，先把该会话切成
    活动会话，动作的反馈才看得见。 */
function searchHere(sessionId: string, prefix: string) {
  if (!prefix) return
  sessions.activeSessionId = sessionId
  void sessions.searchInPrefix(sessionId, prefix)
}

/**
 * 浏览态底部行的比值：「已加载 N / M」，M 是 DBSIZE。
 * 分母拿不到（`dbLists` 里 keys 为 null）或处于搜索态时退回只显示 N。
 * SCAN 是无序采样，部分视图长得和完整视图一模一样 —— 这个比值是
 * 视口里唯一说出「这里不全」的地方（doc/frontend/key-tree.md「诚实信号」）。
 */
function loadedRatio(row: Extract<SidebarRow, { kind: 'more' }>): string {
  const loaded = formatCount(row.loaded)
  return row.total === null ? loaded : `${loaded} / ${formatCount(row.total)}`
}

function isSelected(row: Extract<SidebarRow, { kind: 'node' }>) {
  return sessions.selected?.sessionId === row.sessionId && sessions.selected?.path === row.row.path
}

/* ── 键盘导航 ─────────────────────────────────────────── */

function moveCursor(delta: number) {
  const next = Math.min(rows.value.length - 1, Math.max(0, cursor.value + delta))
  cursor.value = next
  virtualizer.value.scrollToIndex(next, { align: 'auto' })
}

function onKeydown(event: KeyboardEvent) {
  const row = rows.value[cursor.value]
  switch (event.key) {
    case 'ArrowDown':
      event.preventDefault()
      moveCursor(1)
      break
    case 'ArrowUp':
      event.preventDefault()
      moveCursor(-1)
      break
    case 'ArrowRight':
      if (row?.kind === 'node' && row.row.hasChildren && !row.row.expanded) {
        event.preventDefault()
        toggleNode(row)
      }
      break
    case 'ArrowLeft':
      // 合并行只能整体折叠：键盘下选不中中间段，与 VS Code 的限制一致
      if (row?.kind === 'node' && row.row.expanded) {
        event.preventDefault()
        toggleNode(row)
      }
      break
    case 'Enter':
      if (row) {
        event.preventDefault()
        void activate(row)
      }
      break
    default:
      break
  }
}

/* 行数变化后重新测量，否则展开一大片子树时滚动条长度会滞后一帧 */
watch(
  () => rows.value.length,
  () => nextTick(() => virtualizer.value.measure()),
)
</script>

<template>
  <div
    ref="scroller"
    class="tree"
    :class="{
      'no-guides': !settings.state.indentGuides,
      'is-flat': settings.state.defaultTreeMode === 'list',
    }"
    tabindex="0"
    role="tree"
    @keydown="onKeydown"
  >
    <div class="tree-inner" :style="{ height: `${totalSize}px` }">
      <template v-for="{ item, row } in visible" :key="String(item.key)">
        <!-- 连接 -->
        <CtxMenu v-if="row.kind === 'conn'" :width="216">
          <template #trigger>
            <div class="trow lv-conn" :style="rowStyle(item, 0)" @click="activate(row)">
              <span class="tw">
                <!-- 建连可能要好几秒（跨 WAN、TLS 握手、SSH 隧道），
                     没有这个转圈的话点下去界面毫无反应，用户只会重复点击 -->
                <Icon
                  :name="
                    sessions.isConnecting(row.connection.key)
                      ? 'loader'
                      : row.expanded
                        ? 'chevron-down'
                        : 'chevron-right'
                  "
                  :size="12"
                />
              </span>
              <span class="cdot" :style="{ background: row.connection.color || 'var(--fg-3)' }" />
              <span class="tname">{{ row.connection.name }}</span>
              <span class="tbadges">
                <span v-if="row.connection.readonly" class="tag tag-danger">只读</span>
                <span v-if="row.connection.cluster" class="tag">集群</span>
                <span v-if="row.connection.sentinelEnabled" class="tag">哨兵</span>
                <span v-if="row.connection.sshEnabled" class="tag">SSH</span>
                <span v-if="row.connection.tlsEnabled" class="tag">TLS</span>
              </span>
            </div>
          </template>

          <div class="menu-cap clip">{{ row.connection.host }}:{{ row.connection.port }}</div>
          <MenuItem icon="plug" @select="activate(row)">
            {{ row.expanded ? '折叠' : '连接并展开' }}
          </MenuItem>
          <MenuItem icon="pencil" @select="ui.openConnectionDialog(row.connection.key)">
            编辑连接…
          </MenuItem>
          <div class="menu-sep" />
          <MenuItem
            icon="ban"
            :disabled="!row.opened"
            @select="disconnect(row.connection.key)"
          >
            断开
          </MenuItem>
          <MenuItem icon="trash" danger @select="removeConnection(row.connection.key, row.connection.name)">
            删除连接
          </MenuItem>
        </CtxMenu>

        <!-- DB -->
        <CtxMenu v-else-if="row.kind === 'db'" :width="216">
          <template #trigger>
            <div class="trow lv-db" :style="rowStyle(item, 1)" @click="activate(row)">
              <span class="tw">
                <Icon
                  v-if="row.sessionId"
                  :name="row.expanded ? 'chevron-down' : 'chevron-right'"
                  :size="12"
                />
              </span>
              <Icon name="database" :size="12" />
              <span class="tname mono">db{{ row.db }}</span>
              <!-- DB 计数来自 INFO keyspace / DBSIZE，是精确值，不带 `+` -->
              <span v-if="row.keys !== null" class="tcount">{{ formatCount(row.keys) }}</span>
            </div>
          </template>

          <div class="menu-cap mono">db{{ row.db }}</div>
          <MenuItem icon="plus" :disabled="!!row.sessionId && isReadonly(row.sessionId)" @select="createInDb(row)">
            新建 Key…
          </MenuItem>
          <!-- 右键 DB 打开浏览器：作用域 = 该 DB，MATCH `*` -->
          <MenuItem
            icon="list-tree"
            :disabled="!row.sessionId"
            @select="row.sessionId && openBrowserFor(row.sessionId, '')"
          >
            在 Key 浏览器中打开
          </MenuItem>
          <MenuItem
            icon="terminal"
            :disabled="!row.sessionId"
            @select="row.sessionId && tabs.open({ kind: 'cli', sessionId: row.sessionId })"
          >
            命令行
          </MenuItem>
          <div class="menu-sep" />
          <MenuItem
            icon="refresh"
            :disabled="!row.sessionId"
            @select="row.sessionId && sessions.rescan(row.sessionId)"
          >
            重新扫描
          </MenuItem>
          <MenuItem icon="ban" :disabled="!row.sessionId" @select="closeDb(row.sessionId)">
            关闭此数据库
          </MenuItem>
        </CtxMenu>

        <!-- namespace / key -->
        <CtxMenu v-else-if="row.kind === 'node'" :width="248">
          <template #trigger>
            <div
              class="trow"
              :class="[
                row.row.kind === 'key' ? 'lv-key' : 'lv-ns',
                { 'is-sel': isSelected(row), 'is-cursor': cursor === item.index },
              ]"
              :style="rowStyle(item, row.depth)"
              @click="cursor = item.index; activate(row)"
              @dblclick="onRowDblClick(row)"
              @contextmenu.capture="captureSegment($event, row)"
            >
              <span class="tw" @click.stop="toggleNode(row)">
                <Icon
                  v-if="row.row.hasChildren"
                  :name="row.row.expanded ? 'chevron-down' : 'chevron-right'"
                  :size="12"
                />
              </span>

              <TypeDot v-if="row.row.kind === 'key'" :type="typeFor(row)" />
              <Icon v-else :name="row.row.expanded ? 'folder-open' : 'folder'" :size="12" />

              <!-- 左键属于整行：点哪一段都是打开这个 key / 折叠这个命名空间。
                   分段只承担右键 —— 右键 cache 段拿 cache:*，右键 page 段拿 cache:page:*。
                   （合并行里的中间层是单子节点链，本来就没法独立展开，
                   让左键去点它只会是个什么都不发生的空转。） -->
              <span class="tname">
                <template v-for="(segment, index) in row.row.segments" :key="segment.path">
                  <span v-if="index > 0" class="tsep">{{ separatorOf(row.sessionId) }}</span>
                  <span class="tseg" :data-seg-path="segment.path" :data-seg-text="segment.text">{{
                    segment.text
                  }}</span>
                </template>
              </span>

              <!-- TTL 标记。刻意做成和计数不同的视觉：原型里它们同槽位同字重，
                   `23:41` 会被读成"41 个"。这里用带边框的小 chip 区分开，
                   并且它是所有 key 行都有（跟着设置项），不是选中行的孤例。 -->
              <span v-if="ttlOf(row) !== null" class="tttl" :class="{ 'is-soon': isTtlSoon(row) }">
                {{ formatTtl(ttlOf(row)!) }}
              </span>

              <!-- 前缀扫描进行中：计数位换成 spinner（doc/frontend/key-tree.md「前缀权威」）。 -->
              <Icon
                v-if="row.row.hasChildren && prefixBusy(row)"
                class="tspin"
                name="loader"
                :size="12"
              />
              <!-- SCAN 未完成时是部分值，必须带 `+`；前缀权威后摘 `+` 显示精确值。
                   `+` 只有一个字符宽，光靠它解释不了自己 —— 补一句原生 title
                   （树是虚拟化热路径，不为每行计数挂 Tip 组件）。
                   点击即「扫描此前缀」，tooltip 就是它的发现路径。 -->
              <span
                v-else-if="row.row.hasChildren"
                class="tcount"
                :class="{ 'is-action': sessions.canScanPrefix(row.sessionId, row.row.path) }"
                :title="countTitle(row)"
                @click="onCountClick($event, row)"
              >
                {{ formatPartialCount(row.row.keyCount, countExact(row)) }}
              </span>
            </div>
          </template>

          <div class="menu-cap clip">{{ ctxSegment?.path }}</div>

          <template v-if="row.row.kind === 'key'">
            <!-- 右键「打开」是明确意图，直接给驻留页，不进预览 -->
            <MenuItem icon="key" @select="openKey(row.sessionId, row.row.path, typeFor(row), false)">
              打开
            </MenuItem>
            <MenuItem icon="copy" @select="copyText(row.row.path)">复制 Key 名</MenuItem>
            <MenuItem icon="copy" @select="ui.openCloneKey(row.sessionId, row.row.path)">克隆 Key…</MenuItem>
            <!-- 既是 key 又是前缀的节点，或右键落在合并行的中间段（那一段是
                 命名空间）：前缀那套操作也得留着。 -->
            <template v-if="row.row.hasChildren || ctxSegment?.path !== row.row.path">
              <div class="menu-sep" />
              <MenuItem icon="plus" :disabled="isReadonly(row.sessionId)" @select="createInPrefix(row.sessionId, ctxSegment?.path ?? '')">在此前缀下新建 Key…</MenuItem>
              <!-- doc/frontend/key-tree.md「文件夹内搜索与前缀权威」的两个入口，与 namespace 菜单同一组文案 -->
              <MenuItem icon="search" @select="searchHere(row.sessionId, ctxSegment?.path ?? '')">
                在此搜索
              </MenuItem>
              <MenuItem
                icon="refresh"
                :disabled="!sessions.canScanPrefix(row.sessionId, ctxSegment?.path ?? '')"
                :title="scanPrefixHint(row.sessionId, ctxSegment?.path)"
                @select="ctxSegment && sessions.scanPrefix(row.sessionId, ctxSegment.path)"
              >
                扫描此前缀
              </MenuItem>
              <MenuItem icon="list-tree" @select="openBrowserFor(row.sessionId, ctxSegment?.path ?? '')">
                在 Key 浏览器中打开
              </MenuItem>
              <!-- `user` 与 `user:1` 同时存在时，这一行是从树里删掉 `user:*`
                   的唯一入口。不给的话用户只会看到一个「删除」，而它删的是
                   `user` 这一个 key —— 没人能猜到子树得换个地方删。 -->
              <MenuItem
                icon="trash"
                danger
                :disabled="isReadonly(row.sessionId)"
                :title="isReadonly(row.sessionId) ? '该连接为只读模式' : undefined"
                @select="openBatchDeleteFor(row.sessionId, ctxSegment?.path ?? '')"
              >
                批量删除…
              </MenuItem>
            </template>
            <div class="menu-sep" />
            <MenuItem icon="trash" danger @select="emit('deleteKey', row.sessionId, row.row.path)">
              删除
            </MenuItem>
          </template>

          <template v-else>
            <MenuItem icon="plus" :disabled="isReadonly(row.sessionId)" @select="createInPrefix(row.sessionId, ctxSegment?.path ?? '')">在此前缀下新建 Key…</MenuItem>
            <!-- doc/frontend/key-tree.md「文件夹内搜索与前缀权威」的两个入口。「在此搜索」走搜索管线，随时可用；
                 「扫描此前缀」是浏览态的叠加层，搜索态下禁用 ——
                 不允许两条管线同时往一个会话灌 key。 -->
            <MenuItem icon="search" @select="searchHere(row.sessionId, ctxSegment?.path ?? '')">
              在此搜索
            </MenuItem>
            <MenuItem
              icon="refresh"
              :disabled="!sessions.canScanPrefix(row.sessionId, ctxSegment?.path ?? '')"
              :title="scanPrefixHint(row.sessionId, ctxSegment?.path)"
              @select="ctxSegment && sessions.scanPrefix(row.sessionId, ctxSegment.path)"
            >
              扫描此前缀
            </MenuItem>
            <div class="menu-sep" />
            <MenuItem icon="list-tree" @select="openBrowserFor(row.sessionId, ctxSegment?.path ?? '')">
              在 Key 浏览器中打开
            </MenuItem>
            <MenuItem icon="copy" @select="copyText(ctxSegment?.path ?? '')">复制前缀</MenuItem>
            <div class="menu-sep" />
            <!-- v0.1 这里刻意留空：当时没有 batch_ttl 任务类型，
                 画一个点不动的按钮比不画更糟。v0.3 补上了后端，入口才开放。 -->
            <MenuItem
              icon="clock"
              :disabled="isReadonly(row.sessionId)"
              :title="isReadonly(row.sessionId) ? '该连接为只读模式' : undefined"
              @select="openBatchTtlFor(row.sessionId, ctxSegment?.path ?? '')"
            >
              批量设置 TTL…
            </MenuItem>
            <!-- 危险项放最后，和连接菜单里「删除连接」的位置一致。
                 省略号表示它开的是对话框而不是当场就删 —— 单 key 那个「删除」
                 才是按下去就没了。 -->
            <MenuItem
              icon="trash"
              danger
              :disabled="isReadonly(row.sessionId)"
              :title="isReadonly(row.sessionId) ? '该连接为只读模式' : undefined"
              @select="openBatchDeleteFor(row.sessionId, ctxSegment?.path ?? '')"
            >
              批量删除…
            </MenuItem>
          </template>
        </CtxMenu>

        <!-- 精确命中（搜索态置顶行，doc/frontend/key-tree.md「精确命中」）：输入不含 glob
             元字符时并行探测的结果。与普通 key 行同构（类型点 + 等宽 key 名），
             右侧徽标说明它为什么排在最上面；点击就是打开这个 key。 -->
        <CtxMenu v-else-if="row.kind === 'exact'">
          <template #trigger>
        <div
          class="trow lv-key trow-exact"
          :class="{ 'is-cursor': cursor === item.index }"
          :style="rowStyle(item, row.depth)"
          @click="cursor = item.index; openKey(row.sessionId, row.key, row.keyType)"
          @dblclick="openKey(row.sessionId, row.key, row.keyType, false)"
        >
          <span class="tw" />
          <TypeDot :type="row.keyType" />
          <span class="tname"><span class="tseg">{{ row.key }}</span></span>
          <span class="texact">精确命中</span>
        </div>
          </template>
          <MenuItem icon="key" @select="openKey(row.sessionId, row.key, row.keyType, false)">打开</MenuItem>
          <MenuItem icon="copy" @select="copyText(row.key)">复制 Key 名</MenuItem>
          <MenuItem icon="copy" @select="ui.openCloneKey(row.sessionId, row.key)">克隆 Key…</MenuItem>
        </CtxMenu>

        <!-- 加载更多 -->
        <button
          v-else-if="row.kind === 'more'"
          class="trow-more"
          :style="rowStyle(item, row.depth)"
          :disabled="row.loading"
          @click="activate(row)"
        >
          <Icon :name="row.loading ? 'loader' : 'chevron-down'" :size="12" />
          <span v-if="row.loading">正在扫描…</span>
          <span v-else-if="row.truncated">
            已达树节点上限（{{ loadedRatio(row) }}），继续加载
          </span>
          <!-- 浏览态带 DBSIZE 分母：「已加载 20,000 / 2,140,000」。
               两个数在 store 里早就都有（session.keys 与 dbLists），
               只是从来没并排放过 —— 比值是最诚实的一句话。 -->
          <span v-else>加载更多（已加载 {{ loadedRatio(row) }}）</span>
        </button>

        <!-- 搜索态底部行：任务状态替代「加载更多」（doc/frontend/key-tree.md「诚实信号」的文案矩阵）。
             「已找到 0 个 · 扫描中…」与「没有匹配的 Key」是两句不同的陈述 ——
             前者说的是「还没扫到」，绝不能长得像后者。MATCH 模式下拿不到
             「已检查了多少」这个精确分母；百分比是后端从 SCAN 游标估算的
             近似值（doc/backend/tasks.md「扫描任务：只报命中数」），必须带「约」，不可用时保持不定进度。 -->
        <div v-else-if="row.kind === 'search'" class="trow-search" :style="rowStyle(item, row.depth)">
          <template v-if="row.status === 'running'">
            <Icon name="loader" :size="12" />
            <span>已找到 {{ formatCount(row.found) }} 个 · 扫描中…<template v-if="row.progress !== null"> {{ formatApproxPercent(row.progress) }}</template></span>
            <!-- 取消不是危险操作：不丢已有结果，蓝色而不是红色 -->
            <button class="tcancel" type="button" @click="sessions.cancelSearch(row.sessionId)">
              取消
            </button>
          </template>
          <span v-else-if="row.status === 'limited'">
            已达上限（{{ formatCount(row.limit) }}），请细化模式
          </span>
          <span v-else-if="row.status === 'cancelled'">
            已找到 {{ formatCount(row.found) }} 个 · 已取消
          </span>
          <span v-else>已找到 {{ formatCount(row.found) }} 个 · 扫描失败</span>
        </div>

        <!-- 提示 -->
        <div v-else class="trow muted" :style="rowStyle(item, row.depth)">
          <span class="tw" />
          <span class="tname">{{ row.text }}</span>
        </div>
      </template>
    </div>
  </div>
</template>
