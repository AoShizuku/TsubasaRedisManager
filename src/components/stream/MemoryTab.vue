<script setup lang="ts">
import { computed, ref } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import VirtualTable, { type Column } from '@/components/ui/VirtualTable.vue'
import { useTaskStream } from './useTaskStream'
import { startMemoryAnalysis } from '@/api/redis'
import { formatBytes, formatCount } from '@/lib/format'
import { useSettingsStore } from '@/stores/settings'
import { useTabsStore, type Tab } from '@/stores/tabs'
import { useTasksStore } from '@/stores/tasks'

/**
 * 内存分析走后端任务管线。
 *
 * 这条路径在只读连接上照样能跑 —— 逐 key 发 MEMORY USAGE 会被只读守卫
 * 拦掉（MEMORY 不在允许的读命令列表里），而任务管线不经过那道守卫。
 */
const props = defineProps<{ tab: Tab }>()

const settings = useSettingsStore()
const tabs = useTabsStore()
const tasks = useTasksStore()

interface Entry {
  key: string
  size: number
}

const stream = useTaskStream<Entry>({
  limit: 20000,
  map: (item) => (item.type === 'memory' ? { key: item.key, size: item.size } : null),
})

const pattern = ref(props.tab.pattern ?? '*')
const minSize = ref(0)
const maxResults = ref(500)

const columns: Column[] = [
  { key: 'key', label: 'Key', grow: true },
  { key: 'size', label: '内存', width: 130, align: 'right' },
]

/** 后端按到达顺序推送，这里排一次 —— 大 key 排查看的就是排名。 */
const rows = computed(() => [...stream.rows.value].sort((a, b) => b.size - a.size))
const totalSize = computed(() => rows.value.reduce((sum, row) => sum + row.size, 0))

const progress = computed(() =>
  stream.taskId.value ? tasks.tasks.get(stream.taskId.value) ?? null : null,
)

async function run() {
  stream.clear()
  await stream.start(() =>
    startMemoryAnalysis(props.tab.sessionId, {
      pattern: pattern.value || null,
      count: settings.state.scanCount,
      minSize: minSize.value > 0 ? minSize.value : null,
      maxResults: maxResults.value > 0 ? maxResults.value : null,
    }),
  )
}
</script>

<template>
  <div class="toolbar">
    <div class="input-wrap" style="max-width: 260px">
      <span class="lead"><Icon name="search" :size="12" /></span>
      <input v-model="pattern" class="input mono" placeholder="MATCH 模式" :disabled="stream.running.value" />
    </div>
    <span class="muted" style="font-size: var(--fs-11)">最小</span>
    <input v-model.number="minSize" class="input w-88" type="number" min="0" step="1024" :disabled="stream.running.value" />
    <span class="muted" style="font-size: var(--fs-11)">字节 · 上限</span>
    <input v-model.number="maxResults" class="input w-88" type="number" min="10" step="10" :disabled="stream.running.value" />

    <Btn v-if="!stream.running.value" size="sm" variant="primary" icon="chart" @click="run">
      开始分析
    </Btn>
    <Btn v-else size="sm" variant="danger" icon="ban" @click="stream.stop()">停止</Btn>

    <span class="spacer" />
    <span class="muted mono" style="font-size: var(--fs-11)">{{ tabs.scopeLabel(tab.sessionId) }}</span>
  </div>

  <div v-if="progress" class="toolbar" style="background: var(--accent-weak)">
    <Icon name="loader" :size="12" />
    <span class="num" style="font-size: var(--fs-11)">
      已扫描 {{ formatCount(progress.processed) }}
      <template v-if="progress.total"> / {{ formatCount(progress.total) }}</template>
    </span>
    <div class="pbar grow" :class="{ 'is-indeterminate': !progress.total }">
      <i :style="{ width: progress.total ? `${(progress.processed / progress.total) * 100}%` : '35%' }" />
    </div>
  </div>

  <div v-if="!rows.length && !stream.running.value" class="emptyline">
    <Icon name="chart" :size="16" />
    <span>设定范围后开始分析。MEMORY USAGE 是采样估算，不是精确值。</span>
  </div>

  <VirtualTable
    v-else
    :rows="rows"
    :columns="columns"
    :row-key="(row) => row.key"
    :selected-key="null"
  >
    <template #key="{ row }"><span class="k">{{ row.key }}</span></template>
    <template #size="{ row }"><span class="v num">{{ formatBytes(row.size) }}</span></template>
  </VirtualTable>

  <div v-if="rows.length" class="statusbar" style="border-top: 1px solid var(--bd)">
    <span class="sb muted num">
      {{ formatCount(rows.length) }} 个 Key，合计 {{ formatBytes(totalSize) }}
    </span>
    <span class="spacer" />
    <span class="sb muted">结果按 MEMORY USAGE 的采样估算排序</span>
  </div>
</template>
