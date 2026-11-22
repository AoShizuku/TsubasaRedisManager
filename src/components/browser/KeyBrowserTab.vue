<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import Sw from '@/components/ui/Sw.vue'
import TypeDot from '@/components/ui/TypeDot.vue'
import CtxMenu from '@/components/ui/CtxMenu.vue'
import MenuItem from '@/components/ui/MenuItem.vue'
import VirtualTable, { type Column } from '@/components/ui/VirtualTable.vue'
import { useKeyAnalysis } from './useKeyAnalysis'
import { scanKeys } from '@/api/redis'
import { formatBytes, formatCount, formatTtl } from '@/lib/format'
import { keyTypeMeta } from '@/lib/keyTypes'
import { useSessionsStore } from '@/stores/sessions'
import { useSettingsStore } from '@/stores/settings'
import { useTabsStore, type Tab } from '@/stores/tabs'
import { useUiStore } from '@/stores/ui'
import type { KeyEntry } from '@/types/ipc'

/**
 * Key 浏览器：可排序的分析表，放在主区而不是侧栏 —— 268px 塞不下
 * "类型 / TTL / 内存 / 编码"四列，硬塞的结果是每列都截断、排序也失去意义。
 *
 * 分工：树形用来「逛」，这张表用来「查」。
 */
const props = defineProps<{ tab: Tab }>()

const sessions = useSessionsStore()
const settings = useSettingsStore()
const tabs = useTabsStore()
const ui = useUiStore()

const analysis = useKeyAnalysis(props.tab.sessionId)

const pattern = ref(props.tab.pattern ?? '*')
/* 存整个 KeyEntry 而不是裸 key：类型与 TTL 现在随 SCAN 一起回来 */
const keys = ref<KeyEntry[]>([])
const cursor = ref('0')
const complete = ref(false)
const scanning = ref(false)

const sortKey = ref<string>(props.tab.sortByMemory ? 'memory' : 'key')
const sortDesc = ref(!!props.tab.sortByMemory)

/** 勾中的 key。批量操作那一节在下面，但 rescan 要清它，所以声明在这里。 */
const selected = ref(new Set<string>())
const contextRow = ref<Row | null>(null)

const session = computed(() => sessions.sessions.get(props.tab.sessionId))

const columns = computed<Column[]>(() => {
  const base: Column[] = [
    { key: 'key', label: 'Key', grow: true, sortable: true },
    { key: 'type', label: '类型', width: 110, sortable: true },
  ]
  if (!settings.state.analysisMode) return base
  return [
    ...base,
    { key: 'ttl', label: 'TTL', width: 130, align: 'right', sortable: true },
    { key: 'memory', label: '内存', width: 110, align: 'right', sortable: true },
    { key: 'encoding', label: '编码', width: 130 },
  ]
})

interface Row {
  key: string
  type: string | null
  ttl: number | null
  memory: number | null
  encoding: string | null
}

const rows = computed<Row[]>(() => {
  const list: Row[] = keys.value.map((entry) => {
    const info = analysis.data.get(entry.key)
    return {
      key: entry.key,
      type: info?.type ?? entry.keyType,
      ttl: info?.ttl ?? entry.ttl,
      memory: info?.memory ?? null,
      encoding: info?.encoding ?? null,
    }
  })

  const direction = sortDesc.value ? -1 : 1
  list.sort((a, b) => {
    switch (sortKey.value) {
      case 'type':
        return direction * String(a.type ?? '').localeCompare(String(b.type ?? ''))
      case 'ttl':
        // 不过期的排在最后：-1 在数值上最小，但语义上是"最久"
        return direction * (normalizeTtl(a.ttl) - normalizeTtl(b.ttl))
      case 'memory':
        return direction * ((a.memory ?? -1) - (b.memory ?? -1))
      default:
        return direction * a.key.localeCompare(b.key)
    }
  })
  return list
})

function keyNames(): string[] {
  return keys.value.map((entry) => entry.key)
}

/* 存在的 key 一定有 MEMORY USAGE 读数，所以"分析跑完了但一个都没有"
   基本只可能是服务端拒绝了这条命令。注意这不再是本工具只读守卫的锅 ——
   功能路径不经过它，见 doc/backend/sessions.md「只读属于会话」。 */
const memoryUnavailable = computed(() => {
  if (!settings.state.analysisMode) return false
  const loaded = Array.from(analysis.data.values())
  return loaded.length > 0 && loaded.every((entry) => entry.memory === null)
})

function normalizeTtl(value: number | null): number {
  if (value === null) return Number.MAX_SAFE_INTEGER - 1
  if (value < 0) return Number.MAX_SAFE_INTEGER
  return value
}

async function scanMore() {
  if (scanning.value || complete.value) return
  scanning.value = true
  try {
    const result = await scanKeys(
      props.tab.sessionId,
      cursor.value,
      pattern.value || null,
      settings.state.scanCount,
    )
    keys.value.push(...result.keys)
    cursor.value = result.cursor
    complete.value = result.completed
    if (settings.state.analysisMode) void analysis.run(keyNames())
  } catch (error) {
    ui.notifyError(error, '扫描失败')
    complete.value = true
  } finally {
    scanning.value = false
  }
}

async function rescan() {
  keys.value = []
  cursor.value = '0'
  complete.value = false
  // 表都换了一批，旧的勾选没有对应的行了。「加载更多」不清 —— 那是同一批的延续。
  selected.value = new Set()
  analysis.clear()
  await scanMore()
}

onMounted(rescan)

/**
 * 批量任务跑完了就重扫。
 *
 * 批量操作现在是「起个后台任务、对话框立刻关掉」，这张表不再有机会自己
 * `await` 完再刷 —— 只能靠会话的 keyspace 版本号。删除会留下一堆已经不存在的
 * 行，批量 TTL 更隐蔽：TTL 列还显示改之前的读数，而这张表存在的理由正是
 * 「按 TTL 找出快过期的一批」，读数陈旧等于这个功能是错的。
 */
watch(
  () => sessions.revisionOf(props.tab.sessionId),
  () => void rescan(),
)

/** 打开分析模式后要给已经加载的 key 补齐数据，否则排序只对新一批生效。 */
watch(
  () => settings.state.analysisMode,
  (enabled) => {
    if (enabled) void analysis.run(keyNames())
    else analysis.cancel()
  },
)

function onSort(key: string) {
  if (sortKey.value === key) sortDesc.value = !sortDesc.value
  else {
    sortKey.value = key
    sortDesc.value = key === 'memory'
  }
}

/** 和树里一样：单击开预览页，双击转正。在表里翻二十行不该攒下二十个标签页。 */
function openKey(row: Row, preview = true) {
  tabs.open({
    kind: 'key',
    sessionId: props.tab.sessionId,
    key: row.key,
    keyType: row.type,
    preview,
  })
}

/* ── 批量操作 ─────────────────────────────────────────── */

/**
 * 这张表是「查」的地方，查出来要能就地处理 —— 按 TTL 排序找出一批快过期的
 * key，然后续命，否则用户还得把 key 名抄到别处去。见 doc/frontend/batch.md「两个入口」。
 */
const selectedKeys = computed(() => Array.from(selected.value))
const readonly = computed(() => !!session.value?.readonly)

function openBatchTtl() {
  ui.openBatchTtl({
    sessionId: props.tab.sessionId,
    keys: selectedKeys.value,
    pattern: pattern.value,
    scope: 'keys',
  })
}

/**
 * 批量删除。和批量 TTL 走同一个对话框骨架 —— 作用范围、试算、危险文案
 * 都在那里，这里只负责把勾选的 key 递过去。
 *
 * 原来这里自带一个 `ui.confirm` + 直接起任务，等于第二套批量入口：
 * 表格能删显式列表，但表达不了「整个前缀」，而模式恰恰是删除最常见的形状。
 */
function openBatchDelete() {
  ui.openBatchDelete({
    sessionId: props.tab.sessionId,
    keys: selectedKeys.value,
    pattern: pattern.value,
    scope: 'keys',
    // 表格里的模式是用户手输的 glob，不是某个字面前缀，裁不出一片来 ——
    // 这个入口刷新 keyspace 版本号就够了，树本来就是显式加载的快照
    prefix: null,
  })
}
</script>

<template>
  <div class="toolbar">
    <Btn size="sm" icon="plus" :disabled="!session || readonly" @click="ui.openCreateKey(tab.sessionId)">新建 Key</Btn>
    <div class="input-wrap" style="max-width: 320px">
      <span class="lead"><Icon name="search" :size="12" /></span>
      <input
        v-model="pattern"
        class="input mono"
        placeholder="MATCH 模式，例如 cache:page:*"
        @keydown.enter="rescan"
      />
    </div>
    <Btn size="sm" icon="refresh" @click="rescan">扫描</Btn>

    <span class="vsep" />
    <!-- 成本必须显式化：这不是免费默认项 -->
    <Sw v-model="settings.state.analysisMode" size="sm" />
    <span style="font-size: var(--fs-12)">分析模式</span>
    <span class="muted" style="font-size: var(--fs-10)">每 key 额外一次查询</span>

    <span class="spacer" />
    <span class="muted num" style="font-size: var(--fs-11)">
      {{ formatCount(keys.length) }}{{ complete ? '' : '+' }} 个 Key
    </span>
    <Btn
      v-if="!complete"
      size="sm"
      :icon="scanning ? 'loader' : 'chevron-down'"
      :disabled="scanning"
      @click="scanMore"
    >
      {{ scanning ? '扫描中…' : '加载更多' }}
    </Btn>
  </div>

  <div v-if="analysis.running.value" class="toolbar" style="background: var(--accent-weak)">
    <Icon name="loader" :size="12" />
    <span style="font-size: var(--fs-11)">
      正在分析 {{ formatCount(analysis.done.value) }} / {{ formatCount(analysis.total.value) }}
    </span>
    <div class="pbar grow">
      <i :style="{ width: `${(analysis.done.value / Math.max(1, analysis.total.value)) * 100}%` }" />
    </div>
    <Btn size="sm" icon="ban" @click="analysis.cancel()">停止</Btn>
  </div>

  <div v-if="memoryUnavailable" class="banner banner-warn" style="margin: 10px 14px 0">
    <Icon name="lock" :size="16" />
    <span>
      这个服务端没有返回 <code>MEMORY USAGE</code>，内存列因此为空 ——
      常见原因是 ACL 限制或 <code>rename-command</code>。
      需要内存数据请用标签栏的「内存分析」，它走的是后端任务管线。
    </span>
  </div>

  <!-- 扫完了一个都没有：与内存分析 / 订阅页同一种容器内空态，表头也不画 -->
  <div v-if="complete && !keys.length" class="emptyline">
    <span>当前筛选下没有 Key</span>
    <Btn size="sm" icon="plus" :disabled="!session || readonly" @click="ui.openCreateKey(tab.sessionId)">新建 Key</Btn>
  </div>
  <CtxMenu v-else>
    <template #trigger>
  <VirtualTable
    v-model:selected="selected"
    :rows="rows"
    :columns="columns"
    :row-key="(row) => row.key"
    :sort-key="sortKey"
    :sort-desc="sortDesc"
    :selected-key="null"
    selectable
    @sort="onSort"
    @row-click="openKey($event)"
    @row-dbl-click="openKey($event, false)"
    @contextmenu.capture="contextRow = null"
    @row-context-menu="contextRow = $event"
  >
    <template #key="{ row }"><span class="k">{{ row.key }}</span></template>
    <template #type="{ row }">
      <span style="display: inline-flex; align-items: center; gap: 6px">
        <TypeDot :type="row.type" />
        <span class="muted" style="font-size: var(--fs-11)">{{ keyTypeMeta(row.type).label }}</span>
      </span>
    </template>
    <template #ttl="{ row }">
      <span class="v num">{{ row.ttl === null ? '—' : formatTtl(row.ttl) }}</span>
    </template>
    <template #memory="{ row }">
      <span class="v num">{{ row.memory === null ? '—' : formatBytes(row.memory) }}</span>
    </template>
    <template #encoding="{ row }">
      <span class="v">{{ row.encoding ?? '—' }}</span>
    </template>
  </VirtualTable>
    </template>
    <template v-if="contextRow">
      <MenuItem icon="key" @select="openKey(contextRow, false)">打开</MenuItem>
      <MenuItem icon="copy" @select="ui.openCloneKey(tab.sessionId, contextRow.key)">克隆 Key…</MenuItem>
      <div class="menu-sep" />
    </template>
    <MenuItem icon="plus" :disabled="!session || readonly" @select="ui.openCreateKey(tab.sessionId)">新建 Key…</MenuItem>
  </CtxMenu>

  <!-- 勾选后浮出。只读连接下勾选照常可用（选中不是写操作），只有按钮置灰。 -->
  <div v-if="selected.size" class="dt-bulkbar">
    <span class="count">已选 <b>{{ formatCount(selected.size) }}</b> 个 Key</span>
    <!-- 勾的只可能是已加载的行。想对"全部匹配"动手要用模式作用范围 —— 说出来。 -->
    <span class="scope">仅限已加载的 {{ formatCount(rows.length) }} 行</span>

    <span class="spacer" />
    <Btn
      size="sm"
      icon="clock"
      :disabled="readonly"
      :title="readonly ? '该连接为只读模式' : undefined"
      @click="openBatchTtl"
    >
      设置 TTL…
    </Btn>
    <Btn
      size="sm"
      variant="danger"
      icon="trash"
      :disabled="readonly"
      :title="readonly ? '该连接为只读模式' : undefined"
      @click="openBatchDelete"
    >
      删除…
    </Btn>
    <Btn size="sm" variant="ghost" @click="selected = new Set()">取消选择</Btn>
  </div>

  <!-- 对百万级 keyspace 做全量排序需要先全量加载，界面不能假装已经排好了 -->
  <div class="statusbar" style="border-top: 1px solid var(--bd)">
    <span class="sb muted">
      排序只作用于已加载的
      <b class="num">{{ formatCount(keys.length) }}</b>
      个 Key<template v-if="!complete">，扫描尚未完成</template>
    </span>
    <span class="spacer" />
    <span class="sb muted mono">{{ session ? `${session.name} / db${session.db}` : '' }}</span>
  </div>
</template>
