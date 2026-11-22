<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import VirtualTable, { type Column } from '@/components/ui/VirtualTable.vue'
import ValueDrawer from './ValueDrawer.vue'
import { getStreamEntries, getStreamGroups } from '@/api/redis'
import { formatCount } from '@/lib/format'
import { textOf, textValue } from '@/lib/redisText'
import { useUiStore } from '@/stores/ui'
import type { RedisText, StreamEntry, StreamGroup } from '@/types/ipc'

/**
 * Stream 视图。
 *
 * XRANGE 的区间是 ID 而不是下标或游标 —— 这正是六种类型没法共用一个取值命令
 * 的原因之一，见 doc/backend/ipc-contract.md「不做」。
 * 区间端点、XREVRANGE 的参数反序都由后端处理。
 */
const props = defineProps<{ sessionId: string; keyName: string; readonly: boolean }>()

const ui = useUiStore()

const PAGE = 100

const tab = ref<'entries' | 'groups'>('entries')
const entries = ref<StreamEntry[]>([])
const groups = ref<StreamGroup[]>([])
const complete = ref(false)
const loading = ref(false)
const selected = ref<string | null>(null)
const reversed = ref(true)

const selectedEntry = computed(() => entries.value.find((entry) => entry.id === selected.value))

const entryColumns: Column[] = [
  { key: 'id', label: 'ID', width: 200 },
  { key: 'fields', label: '字段', grow: true },
  { key: 'count', label: '字段数', width: 80, align: 'right' },
]

const groupColumns: Column[] = [
  { key: 'name', label: '消费组', width: 220 },
  { key: 'consumers', label: '消费者', width: 90, align: 'right' },
  { key: 'pending', label: '待确认', width: 90, align: 'right' },
  { key: 'lastDeliveredId', label: '最后投递 ID', grow: true },
]

async function loadMore() {
  if (loading.value || complete.value) return
  loading.value = true
  try {
    // 默认按时间倒序：Stream 最有价值的是最近的条目。
    // 续页从上一批的边界 ID 开排他，`(` 前缀就是 Redis 的开区间语法。
    const last = entries.value[entries.value.length - 1]?.id
    const start = last && !reversed.value ? `(${last}` : '-'
    const end = last && reversed.value ? `(${last}` : '+'

    const page = await getStreamEntries(
      props.sessionId,
      props.keyName,
      start,
      end,
      PAGE,
      reversed.value,
    )
    entries.value.push(...page.entries)
    complete.value = page.entries.length < PAGE
  } catch (error) {
    ui.notifyError(error, '读取条目失败')
    complete.value = true
  } finally {
    loading.value = false
  }
}

async function loadGroups() {
  try {
    groups.value = await getStreamGroups(props.sessionId, props.keyName)
  } catch {
    // 没有消费组，或者服务端版本不支持 XINFO GROUPS
    groups.value = []
  }
}

async function reload() {
  entries.value = []
  complete.value = false
  selected.value = null
  await Promise.all([loadMore(), loadGroups()])
}

onMounted(reload)

async function toggleOrder() {
  reversed.value = !reversed.value
  await reload()
}

/** 一行放不下全部字段，先给一条压平的预览，完整内容在下面的抽屉里。 */
function fieldSummary(entry: StreamEntry): string {
  return entry.fields.map((f) => `${textOf(f.field)}=${textOf(f.value)}`).join('  ')
}

/**
 * 抽屉里放的是**整条 entry** 拼出来的文本，不是单个值 —— 选中粒度是 entry。
 *
 * 所以这里的解码按钮是个死按钮：即使字段值没被截断、即使后端加了重取，
 * 送过去的也是 `field1\n  value1\n\nfield2\n  value2` 这么一坨，永远解不开。
 * 关掉它比留着一个必然失败的按钮诚实。字段级选中是一个交互改动，
 * 见 doc/backlog.md「Stream 里的字段值解不了码」。
 */
const drawerValue = computed<RedisText | null>(() => {
  const entry = selectedEntry.value
  if (!entry) return null
  return textValue(entry.fields.map((f) => `${textOf(f.field)}\n  ${textOf(f.value)}`).join('\n\n'))
})
</script>

<template>
  <div class="subtabs">
    <button class="subtab" :class="{ 'is-active': tab === 'entries' }" @click="tab = 'entries'">
      条目
      <span class="count-pill">{{ formatCount(entries.length) }}{{ complete ? '' : '+' }}</span>
    </button>
    <button class="subtab" :class="{ 'is-active': tab === 'groups' }" @click="tab = 'groups'">
      消费组
      <span class="count-pill">{{ groups.length }}</span>
    </button>
  </div>

  <template v-if="tab === 'entries'">
    <div class="toolbar">
      <Btn size="sm" :icon="reversed ? 'arrow-down' : 'arrow-up'" @click="toggleOrder">
        {{ reversed ? '最新在前' : '最早在前' }}
      </Btn>
      <span class="muted num" style="font-size: var(--fs-11)">
        已加载 {{ formatCount(entries.length) }}
      </span>
      <Btn
        v-if="!complete"
        size="sm"
        :icon="loading ? 'loader' : 'chevron-down'"
        :disabled="loading"
        @click="loadMore"
      >
        {{ loading ? '加载中…' : '加载更多' }}
      </Btn>
      <span class="spacer" />
      <Btn size="sm" icon="refresh" @click="reload">刷新</Btn>
    </div>

    <VirtualTable
      :rows="entries"
      :columns="entryColumns"
      :row-key="(row) => row.id"
      :selected-key="selected"
      @row-click="selected = $event.id"
    >
      <!-- Stream ID 是数据，必须等宽 -->
      <template #id="{ row }">
        <span class="k">{{ row.id }}</span>
      </template>
      <template #fields="{ row }">
        <span class="v">{{ fieldSummary(row) }}</span>
      </template>
      <template #count="{ row }">
        <span class="v num">{{ row.fields.length }}</span>
      </template>
    </VirtualTable>

    <!-- decodable=false：这里是整条 entry 的拼接，没有单个值可解，见上面 drawerValue 的注释 -->
    <ValueDrawer
      :title="selectedEntry ? `条目 ${selectedEntry.id}` : '条目详情'"
      :value="drawerValue"
      :decodable="false"
    />
  </template>

  <template v-else>
    <div v-if="!groups.length" class="emptyline">
      <Icon name="bell" :size="16" />
      <span>这个 Stream 没有消费组</span>
    </div>
    <VirtualTable
      v-else
      :rows="groups"
      :columns="groupColumns"
      :row-key="(row) => row.name"
      :selected-key="null"
    >
      <template #name="{ row }"><span class="k">{{ row.name }}</span></template>
      <template #consumers="{ row }"><span class="v num">{{ row.consumers }}</span></template>
      <template #pending="{ row }">
        <span class="v num" :style="row.pending > 0 ? 'color: var(--warn)' : undefined">
          {{ row.pending }}
        </span>
      </template>
      <template #lastDelivered="{ row }"><span class="v">{{ row.lastDeliveredId }}</span></template>
    </VirtualTable>
  </template>
</template>
