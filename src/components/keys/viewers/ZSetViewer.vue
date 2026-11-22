<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import VirtualTable, { type Column } from '@/components/ui/VirtualTable.vue'
import CollectionToolbar from './CollectionToolbar.vue'
import ValueDrawer from './ValueDrawer.vue'
import ValuePreview from './ValuePreview.vue'
import { useRangeCollection } from './useRangeCollection'
import { addZSetMember, deleteZSetMembers, getZSetRange } from '@/api/redis'
import {
  base64FromText,
  base64Of,
  isBinaryText,
  memberLocateBlockedReason,
  textOf,
} from '@/lib/redisText'
import { useSettingsStore } from '@/stores/settings'
import { useUiStore } from '@/stores/ui'
import type { ZSetMember } from '@/types/ipc'

const props = defineProps<{
  sessionId: string
  keyName: string
  total: number | null
  readonly: boolean
}>()
const emit = defineEmits<{ changed: [] }>()

const settings = useSettingsStore()
const ui = useUiStore()

const desc = ref(false)

/* 分数序是 ZSet 的语义，所以主视图按下标翻页而不是 ZSCAN —— SCAN 给不了顺序。
   RESP2 的扁平数组与 RESP3 的成对数组差异已经在后端吃掉，
   这里不再需要 stride，也不用嗅探回复形状。 */
const collection = useRangeCollection<ZSetMember>(async (start, stop) => {
  const page = await getZSetRange(props.sessionId, props.keyName, start, stop, desc.value)
  return { items: page.members, total: page.total }
})

interface Row {
  member: string
  raw: ZSetMember
  /** 分数按字符串保留：inf / -inf 是合法分数，转成数字会丢掉它们 */
  score: string
}

const rows = computed<Row[]>(() =>
  collection.items.value.map((entry) => ({
    member: textOf(entry.member),
    raw: entry,
    score: entry.score,
  })),
)

const columns: Column[] = [
  { key: 'member', label: '成员', grow: true },
  { key: 'score', label: '分数', width: 140, align: 'right' },
  { key: 'actions', label: '', width: 68, align: 'right' },
]

const selected = ref<string | null>(null)
const selectedRow = computed(() => rows.value.find((row) => row.member === selected.value) ?? null)

onMounted(() => collection.reset())

/* Redis 的分数是 double，inf / -inf / +inf 都是合法输入，
   所以不能简单用 Number.isFinite 挡掉。 */
function isValidScore(value: string): boolean {
  const trimmed = value.trim().toLowerCase()
  if (['inf', '+inf', '-inf', 'infinity', '+infinity', '-infinity'].includes(trimmed)) return true
  return trimmed !== '' && Number.isFinite(Number(trimmed))
}

async function toggleOrder() {
  desc.value = !desc.value
  await collection.reset()
}

async function editMember(row: Row) {
  const blocked = memberLocateBlockedReason(row.raw.member)
  if (blocked) {
    ui.notify('warn', blocked)
    return
  }
  // 非 UTF-8 的成员在编辑框里是一串十六进制，改了它等于建一个以十六进制文本命名的
  // 新成员。所以只给改分数，成员原样用原始字节。
  const binary = isBinaryText(row.raw.member)
  const scoreField = { name: 'score', label: '分数', value: row.score, mono: true }
  const result = await ui.prompt({
    title: binary ? '修改分数' : '编辑成员',
    fields: binary
      ? [scoreField]
      : [{ name: 'member', label: '成员', value: row.member, multiline: true }, scoreField],
  })
  if (!result) return
  if (!isValidScore(result.score)) {
    ui.notify('warn', '分数必须是数字，或 inf / -inf')
    return
  }
  const renamed = !binary && result.member !== row.member
  try {
    if (renamed) {
      await deleteZSetMembers(props.sessionId, props.keyName, [base64Of(row.raw.member)])
    }
    await addZSetMember(
      props.sessionId,
      props.keyName,
      // 没改名就送回原始字节，而不是 `textOf()` 的产物再编码一遍
      renamed ? base64FromText(result.member) : base64Of(row.raw.member),
      result.score,
    )
    await collection.reset()
    emit('changed')
    ui.notify('ok', '已保存')
  } catch (error) {
    ui.notifyError(error, '保存失败')
  }
}

async function addMember() {
  const result = await ui.prompt({
    title: '新增成员',
    fields: [
      { name: 'member', label: '成员', multiline: true },
      { name: 'score', label: '分数', value: '0', mono: true },
    ],
  })
  if (!result?.member) return
  if (!isValidScore(result.score)) {
    ui.notify('warn', '分数必须是数字，或 inf / -inf')
    return
  }
  try {
    await addZSetMember(
      props.sessionId,
      props.keyName,
      base64FromText(result.member),
      result.score,
    )
    await collection.reset()
    emit('changed')
  } catch (error) {
    ui.notifyError(error, '新增失败')
  }
}

async function removeMember(row: Row) {
  const blocked = memberLocateBlockedReason(row.raw.member)
  if (blocked) {
    ui.notify('warn', blocked)
    return
  }
  if (settings.state.confirmDelete) {
    const ok = await ui.confirm({
      title: '删除成员',
      body: '删除后无法恢复。',
      detail: row.member,
      confirmText: '删除',
      danger: true,
    })
    if (!ok) return
  }
  try {
    await deleteZSetMembers(props.sessionId, props.keyName, [base64Of(row.raw.member)])
    if (selected.value === row.member) selected.value = null
    await collection.reset()
    emit('changed')
  } catch (error) {
    ui.notifyError(error, '删除失败')
  }
}
</script>

<template>
  <CollectionToolbar
    :loaded="rows.length"
    :total="total"
    :complete="collection.complete.value"
    :loading="collection.loading.value"
    :readonly="readonly"
    :searchable="false"
    add-label="新增成员"
    @load-more="collection.loadMore()"
    @add="addMember"
  >
  </CollectionToolbar>

  <VirtualTable
    :rows="rows"
    :columns="columns"
    :row-key="(row) => row.member"
    :selected-key="selected"
    :sort-key="'score'"
    :sort-desc="desc"
    @row-click="selected = $event.member"
    @sort="toggleOrder"
  >
    <template #member="{ row }">
      <ValuePreview :value="row.raw.member" />
    </template>
    <!-- 分数右对齐 + tabular-nums：比较大小时数字必须成列 -->
    <template #score="{ row }">
      <span class="v num">{{ row.score }}</span>
    </template>
    <template #actions="{ row }">
      <span class="rowact">
        <Btn
          variant="ghost"
          size="sm"
          icon="pencil"
          icon-only
          :disabled="readonly || memberLocateBlockedReason(row.raw.member) !== null"
          :title="memberLocateBlockedReason(row.raw.member) ?? '编辑'"
          @click.stop="editMember(row)"
        />
        <Btn
          variant="ghost"
          size="sm"
          icon="trash"
          icon-only
          :disabled="readonly || memberLocateBlockedReason(row.raw.member) !== null"
          :title="memberLocateBlockedReason(row.raw.member) ?? '删除'"
          @click.stop="removeMember(row)"
        />
      </span>
    </template>
  </VirtualTable>

  <!-- 与 Set 同理：成员既是值又是定位符，重取不了。见 doc/backend/ipc-contract.md「完整值重取」 -->
  <ValueDrawer
    title="成员详情"
    :value="selectedRow ? selectedRow.raw.member : null"
    no-target-reason="集合成员没有独立的定位符，无法重取完整值 —— 成员本身就是值，要精确定位它得先有完整成员。"
  />
</template>
