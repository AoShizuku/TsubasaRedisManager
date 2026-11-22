<script setup lang="ts">
import { computed, onMounted } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import VirtualTable, { type Column } from '@/components/ui/VirtualTable.vue'
import { useTaskStream } from './useTaskStream'
import { startMonitor } from '@/api/redis'
import { formatCount } from '@/lib/format'
import { useTabsStore, type Tab } from '@/stores/tabs'

const props = defineProps<{ tab: Tab }>()
const tabs = useTabsStore()

interface Line {
  id: number
  timestamp: string
  database: number | null
  source: string | null
  command: string
}

let lineId = 0

const stream = useTaskStream<Line>({
  limit: 5000,
  map: (item) =>
    item.type === 'monitor'
      ? {
          id: (lineId += 1),
          timestamp: item.timestamp,
          database: item.database,
          source: item.source,
          command: item.args.join(' ') || item.raw,
        }
      : null,
})

const columns: Column[] = [
  { key: 'timestamp', label: '时间', width: 150 },
  { key: 'source', label: '来源', width: 150 },
  { key: 'database', label: 'DB', width: 60, align: 'right' },
  { key: 'command', label: '命令', grow: true },
]

onMounted(() => stream.start(() => startMonitor(props.tab.sessionId)))

const rows = computed(() => stream.rows.value)
</script>

<template>
  <div class="toolbar">
    <span class="dot" :class="stream.running.value ? 'dot-ok' : 'dot-off'" />
    <span style="font-size: var(--fs-12)">
      {{ stream.running.value ? 'MONITOR 运行中' : '已停止' }}
    </span>
    <span class="muted mono" style="font-size: var(--fs-11)">
      {{ tabs.scopeLabel(tab.sessionId) }}
    </span>

    <span class="vsep" />
    <Btn
      v-if="stream.running.value"
      size="sm"
      :icon="stream.paused.value ? 'play' : 'square'"
      @click="stream.paused.value = !stream.paused.value"
    >
      {{ stream.paused.value ? '继续' : '暂停' }}
    </Btn>
    <Btn
      v-if="stream.running.value"
      size="sm"
      variant="danger"
      icon="ban"
      @click="stream.stop()"
    >
      停止
    </Btn>
    <Btn
      v-else
      size="sm"
      icon="play"
      @click="stream.start(() => startMonitor(tab.sessionId))"
    >
      重新开始
    </Btn>
    <Btn size="sm" icon="rotate-ccw" @click="stream.clear()">清空</Btn>

    <span class="spacer" />
    <span class="muted num" style="font-size: var(--fs-11)">{{ formatCount(rows.length) }} 条</span>
  </div>

  <div class="banner banner-warn" style="margin: 10px 14px 0">
    <Icon name="alert-triangle" :size="16" />
    <span>
      MONITOR 会让服务端把每条命令都推送出来，在生产库上会显著拖慢吞吐。用完请停止。
    </span>
  </div>

  <!-- 只保留最近 5000 条：挂一夜的 Monitor 否则会把内存吃光 -->
  <div v-if="stream.dropped.value > 0" class="toolbar" style="background: var(--bg-inset)">
    <span class="muted" style="font-size: var(--fs-11)">
      已丢弃最早的 {{ formatCount(stream.dropped.value) }} 条，仅保留最近 5000 条
    </span>
  </div>

  <VirtualTable :rows="rows" :columns="columns" :row-key="(row) => row.id" :selected-key="null">
    <template #timestamp="{ row }"><span class="v num">{{ row.timestamp }}</span></template>
    <template #source="{ row }"><span class="v">{{ row.source ?? '—' }}</span></template>
    <template #database="{ row }"><span class="v num">{{ row.database ?? '—' }}</span></template>
    <template #command="{ row }"><span class="k">{{ row.command }}</span></template>
  </VirtualTable>
</template>
