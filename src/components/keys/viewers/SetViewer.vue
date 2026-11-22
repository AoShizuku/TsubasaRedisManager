<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import VirtualTable, { type Column } from '@/components/ui/VirtualTable.vue'
import CollectionToolbar from './CollectionToolbar.vue'
import ValueDrawer from './ValueDrawer.vue'
import ValuePreview from './ValuePreview.vue'
import { useScanCollection } from './useScanCollection'
import { addSetMembers, deleteSetMembers, getSetMembers } from '@/api/redis'
import { base64FromText, base64Of, memberLocateBlockedReason, textOf } from '@/lib/redisText'
import { useSettingsStore } from '@/stores/settings'
import { useUiStore } from '@/stores/ui'
import type { RedisText } from '@/types/ipc'

const props = defineProps<{ sessionId: string; keyName: string; readonly: boolean }>()
const emit = defineEmits<{ changed: [] }>()

const settings = useSettingsStore()
const ui = useUiStore()
const collection = useScanCollection<RedisText>(async (cursor, pattern, count) => {
  const page = await getSetMembers(props.sessionId, props.keyName, cursor, pattern || null, count)
  return { items: page.members, cursor: page.cursor, completed: page.completed }
})

interface Row {
  member: string
  raw: RedisText
}

const rows = computed<Row[]>(() =>
  collection.items.value.map((raw) => ({ member: textOf(raw), raw })),
)

const columns: Column[] = [
  { key: 'member', label: '成员', grow: true },
  { key: 'actions', label: '', width: 40, align: 'right' },
]

const selected = ref<string | null>(null)
const selectedRow = computed(() => rows.value.find((row) => row.member === selected.value) ?? null)

onMounted(() => collection.reset())

async function addMember() {
  const result = await ui.prompt({
    title: '新增成员',
    fields: [{ name: 'member', label: '成员', multiline: true }],
  })
  if (!result?.member) return
  try {
    await addSetMembers(props.sessionId, props.keyName, [base64FromText(result.member)])
    await collection.reset()
    emit('changed')
  } catch (error) {
    ui.notifyError(error, '新增失败')
  }
}

async function removeMember(row: Row) {
  const blocked = memberLocateBlockedReason(row.raw)
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
    // 用原始字节删：成员可能不是 UTF-8，按显示出来的十六进制去删是删不掉的
    await deleteSetMembers(props.sessionId, props.keyName, [base64Of(row.raw)])
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
    :complete="collection.complete.value"
    :loading="collection.loading.value"
    :readonly="readonly"
    search-placeholder="在成员中搜索"
    add-label="新增成员"
    @search="collection.reset($event)"
    @load-more="collection.loadMore()"
    @add="addMember"
  />

  <VirtualTable
    :rows="rows"
    :columns="columns"
    :row-key="(row) => row.member"
    :selected-key="selected"
    @row-click="selected = $event.member"
  >
    <template #member="{ row }">
      <ValuePreview :value="row.raw" />
    </template>
    <template #actions="{ row }">
      <span class="rowact">
        <Btn
          variant="ghost"
          size="sm"
          icon="trash"
          icon-only
          :disabled="readonly || memberLocateBlockedReason(row.raw) !== null"
          :title="memberLocateBlockedReason(row.raw) ?? '删除'"
          @click.stop="removeMember(row)"
        />
      </span>
    </template>
  </VirtualTable>

  <!--
    没有 target：集合成员既是值又是定位符，重取不了。这是 Redis 的数据模型而不是
    实现偷懒，所以不能只把按钮禁掉，得说出原因。见 doc/backend/ipc-contract.md「完整值重取」。
  -->
  <ValueDrawer
    title="成员详情"
    :value="selectedRow ? selectedRow.raw : null"
    no-target-reason="集合成员没有独立的定位符，无法重取完整值 —— 成员本身就是值，要精确定位它得先有完整成员。"
  />
</template>
