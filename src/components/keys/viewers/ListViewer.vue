<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import VirtualTable, { type Column } from '@/components/ui/VirtualTable.vue'
import CollectionToolbar from './CollectionToolbar.vue'
import ValueDrawer from './ValueDrawer.vue'
import ValuePreview from './ValuePreview.vue'
import { useRangeCollection } from './useRangeCollection'
import { deleteListItem, getListRange, pushListItem, setListItem } from '@/api/redis'
import { capabilityBlockedReason } from '@/lib/capabilities'
import { base64FromText, textEditBlockedReason, textOf } from '@/lib/redisText'
import { useSessionsStore } from '@/stores/sessions'
import { useUiStore } from '@/stores/ui'
import type { RedisText, ValueRef } from '@/types/ipc'

const props = defineProps<{
  sessionId: string
  keyName: string
  total: number | null
  readonly: boolean
}>()
const emit = defineEmits<{ changed: [] }>()

const ui = useUiStore()
const sessions = useSessionsStore()

/** 按下标删除是 Lua 脚本；服务端没有脚本能力时按钮置灰并写明原因。 */
const deleteBlocked = computed(() =>
  capabilityBlockedReason(sessions.sessions.get(props.sessionId)?.capabilities, 'scripting'),
)

/* List 必须按下标读，顺序就是它的语义 */
const collection = useRangeCollection<RedisText>(async (start, stop) => {
  const page = await getListRange(props.sessionId, props.keyName, start, stop)
  return { items: page.items, total: page.total }
})

interface Row {
  index: number
  raw: RedisText
}

const rows = computed<Row[]>(() =>
  collection.items.value.map((raw, index) => ({ index, raw })),
)

const columns: Column[] = [
  { key: 'index', label: '#', width: 64, align: 'right' },
  { key: 'value', label: '值', grow: true },
  { key: 'actions', label: '', width: 68, align: 'right' },
]

const selected = ref<number | null>(null)
const selectedRow = computed(() =>
  selected.value === null ? null : rows.value[selected.value] ?? null,
)

/* 分页里的元素按 64 KiB 截断，抽屉靠下标重取完整值 —— `row.index` 就是
   整个列表里的绝对下标（页是从 0 累加上来的），LINDEX 直接吃它 */
const drawerTarget = computed<ValueRef | null>(() =>
  selectedRow.value
    ? { kind: 'listIndex', key: props.keyName, index: selectedRow.value.index }
    : null,
)

onMounted(() => collection.reset())

async function editItem(row: Row) {
  const blocked = textEditBlockedReason(row.raw)
  if (blocked) {
    ui.notify('warn', blocked, '在值详情里加载完整值或解码后查看，修改请走 CLI 标签页。')
    return
  }
  const result = await ui.prompt({
    title: `编辑第 ${row.index} 项`,
    fields: [{ name: 'value', label: '值', value: textOf(row.raw), multiline: true }],
  })
  if (!result) return
  try {
    await setListItem(props.sessionId, props.keyName, row.index, base64FromText(result.value))
    await collection.reset()
    emit('changed')
    ui.notify('ok', '已保存')
  } catch (error) {
    ui.notifyError(error, '保存失败')
  }
}

async function push(tail: boolean) {
  const result = await ui.prompt({
    title: tail ? '从尾部追加' : '从头部插入',
    fields: [{ name: 'value', label: '值', multiline: true }],
  })
  if (!result) return
  try {
    await pushListItem(props.sessionId, props.keyName, base64FromText(result.value), tail)
    await collection.reset()
    emit('changed')
  } catch (error) {
    ui.notifyError(error, '插入失败')
  }
}

/**
 * 按下标删除，截断与非 UTF-8 的元素也能删：定位符是下标，不是元素内容。
 *
 * List 没有「按下标删除」这条命令，后端用 Lua 把「改写成哨兵 + LREM」包成原子操作
 * （`values.rs` 的 `DELETE_LIST_ITEM_SCRIPT`），所以确认框里不需要「可能删错元素」的免责声明。
 */
async function removeItem(row: Row) {
  const ok = await ui.confirm({
    title: '删除该项',
    body: '删除后无法恢复。',
    detail: textOf(row.raw),
    confirmText: '删除',
    danger: true,
  })
  if (!ok) return

  try {
    const deleted = await deleteListItem(props.sessionId, props.keyName, row.index)
    if (!deleted) ui.notify('warn', '该位置已经不存在，列表可能刚被别处改过')
    selected.value = null
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
    add-label="追加"
    @load-more="collection.loadMore()"
    @add="push(true)"
  />

  <VirtualTable
    :rows="rows"
    :columns="columns"
    :row-key="(row) => row.index"
    :selected-key="selected"
    @row-click="selected = $event.index"
  >
    <template #index="{ row }">
      <span class="idx num">{{ row.index }}</span>
    </template>
    <template #value="{ row }">
      <ValuePreview :value="row.raw" />
    </template>
    <template #actions="{ row }">
      <span class="rowact">
        <Btn
          variant="ghost"
          size="sm"
          icon="pencil"
          icon-only
          :disabled="readonly || textEditBlockedReason(row.raw) !== null"
          :title="textEditBlockedReason(row.raw) ?? '编辑'"
          @click.stop="editItem(row)"
        />
        <Btn
          variant="ghost"
          size="sm"
          icon="trash"
          icon-only
          :disabled="readonly || deleteBlocked !== null"
          :title="deleteBlocked ?? '删除'"
          @click.stop="removeItem(row)"
        />
      </span>
    </template>
  </VirtualTable>

  <ValueDrawer
    :title="selectedRow ? `第 ${selectedRow.index} 项` : '值详情'"
    :value="selectedRow ? selectedRow.raw : null"
    :session-id="sessionId"
    :target="drawerTarget"
  />
</template>
