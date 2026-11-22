<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import VirtualTable, { type Column } from '@/components/ui/VirtualTable.vue'
import CollectionToolbar from './CollectionToolbar.vue'
import ValueDrawer from './ValueDrawer.vue'
import ValuePreview from './ValuePreview.vue'
import { useScanCollection } from './useScanCollection'
import { deleteHashFields, getHashEntries, setHashField } from '@/api/redis'
import {
  base64FromText,
  base64Of,
  isBinaryText,
  textEditBlockedReason,
  textOf,
} from '@/lib/redisText'
import { useSettingsStore } from '@/stores/settings'
import { useUiStore } from '@/stores/ui'
import type { HashEntry, ValueRef } from '@/types/ipc'

const props = defineProps<{ sessionId: string; keyName: string; readonly: boolean }>()
const emit = defineEmits<{ changed: [] }>()

const settings = useSettingsStore()
const ui = useUiStore()
const collection = useScanCollection<HashEntry>(async (cursor, pattern, count) => {
  const page = await getHashEntries(props.sessionId, props.keyName, cursor, pattern || null, count)
  return { items: page.entries, cursor: page.cursor, completed: page.completed }
})

interface Row {
  field: string
  entry: HashEntry
}

/* RESP2 的扁平数组与 RESP3 的 map 差异已经在后端吃掉了，这里拿到的就是字段对 */
const rows = computed<Row[]>(() =>
  collection.items.value.map((entry) => ({ field: textOf(entry.field), entry })),
)

const columns: Column[] = [
  { key: 'field', label: '字段', width: 260 },
  { key: 'value', label: '值', grow: true },
  { key: 'actions', label: '', width: 68, align: 'right' },
]

const selected = ref<string | null>(null)
const selectedRow = computed(() => rows.value.find((row) => row.field === selected.value) ?? null)

/**
 * 分页里的字段值按 64 KiB 截断（`MEMBER_VALUE_LIMIT`），抽屉靠这个定位符重取完整值。
 *
 * 字段名走 `base64Of` 而不是 `base64FromText(row.field)`：`row.field` 是
 * `textOf()` 的产物，字段名不是 UTF-8 时它是一串十六进制，拿去 HGET 取不到东西。
 */
const drawerTarget = computed<ValueRef | null>(() =>
  selectedRow.value
    ? {
        kind: 'hashField',
        key: props.keyName,
        fieldBase64: base64Of(selectedRow.value.entry.field),
      }
    : null,
)

onMounted(() => collection.reset())

/**
 * 这一行能不能走文本编辑框，不能的话原因是什么。字段名也要查：它同样是
 * `textOf()` 的产物，非 UTF-8 时照着存回去会建出一个名叫 `1f 8b 08` 的字段。
 */
function uneditableReason(row: Row): string | null {
  const blocked = textEditBlockedReason(row.entry.value)
  if (blocked) return blocked
  if (isBinaryText(row.entry.field)) return '字段名不是 UTF-8，编辑框里显示的是十六进制'
  return null
}

async function editField(row: Row) {
  const blocked = uneditableReason(row)
  if (blocked) {
    ui.notify('warn', blocked, '在值详情里解码后再改，或走 CLI 标签页。')
    return
  }
  const result = await ui.prompt({
    title: '编辑字段',
    fields: [
      { name: 'field', label: '字段', value: row.field, mono: true },
      { name: 'value', label: '值', value: textOf(row.entry.value), multiline: true },
    ],
  })
  if (!result) return
  const renamed = result.field !== row.field
  try {
    // 改了字段名等于删旧建新：HSET 不会替换 key，旧字段会留下
    if (renamed) {
      await deleteHashFields(props.sessionId, props.keyName, [base64Of(row.entry.field)])
    }
    await setHashField(
      props.sessionId,
      props.keyName,
      // 没改名就送回原始字节。`row.field` 是 `textOf()` 的产物，拿它当字段名
      // 会 HSET 出**另一个**字段而不是更新这一个。
      renamed ? base64FromText(result.field) : base64Of(row.entry.field),
      base64FromText(result.value),
    )
    await collection.reset()
    emit('changed')
    ui.notify('ok', '已保存')
  } catch (error) {
    ui.notifyError(error, '保存失败')
  }
}

async function addField() {
  const result = await ui.prompt({
    title: '新增字段',
    fields: [
      { name: 'field', label: '字段', mono: true },
      { name: 'value', label: '值', multiline: true },
    ],
  })
  if (!result?.field) return
  try {
    await setHashField(
      props.sessionId,
      props.keyName,
      base64FromText(result.field),
      base64FromText(result.value),
    )
    await collection.reset()
    emit('changed')
  } catch (error) {
    ui.notifyError(error, '新增失败')
  }
}

async function removeField(row: Row) {
  if (settings.state.confirmDelete) {
    const ok = await ui.confirm({
      title: '删除字段',
      body: '删除后无法恢复。',
      detail: row.field,
      confirmText: '删除',
      danger: true,
    })
    if (!ok) return
  }
  try {
    // 原始字节，不是界面上那串十六进制 —— 后者 HDEL 谁也删不掉
    await deleteHashFields(props.sessionId, props.keyName, [base64Of(row.entry.field)])
    if (selected.value === row.field) selected.value = null
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
    :complete="collection.complete.value"
    :loading="collection.loading.value"
    :readonly="readonly"
    search-placeholder="在字段名中搜索"
    add-label="新增字段"
    @search="collection.reset($event)"
    @load-more="collection.loadMore()"
    @add="addField"
  />

  <VirtualTable
    :rows="rows"
    :columns="columns"
    :row-key="(row) => row.field"
    :selected-key="selected"
    @row-click="selected = $event.field"
  >
    <template #field="{ row }">
      <span class="k">{{ row.field }}</span>
    </template>
    <template #value="{ row }">
      <ValuePreview :value="row.entry.value" />
    </template>
    <template #actions="{ row }">
      <span class="rowact">
        <!-- 不能编辑的行直接禁用并写明原因，不要等用户点下去才报错 -->
        <Btn
          variant="ghost"
          size="sm"
          icon="pencil"
          icon-only
          :disabled="readonly || uneditableReason(row) !== null"
          :title="uneditableReason(row) ?? '编辑'"
          @click.stop="editField(row)"
        />
        <Btn
          variant="ghost"
          size="sm"
          icon="trash"
          icon-only
          :disabled="readonly"
          title="删除"
          @click.stop="removeField(row)"
        />
      </span>
    </template>
  </VirtualTable>

  <ValueDrawer
    :title="selectedRow ? `字段 ${selectedRow.field}` : '值详情'"
    :value="selectedRow ? selectedRow.entry.value : null"
    :session-id="sessionId"
    :target="drawerTarget"
  />
</template>
