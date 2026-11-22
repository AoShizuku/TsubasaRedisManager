<script setup lang="ts" generic="T">
import { computed, nextTick, ref, watch } from 'vue'
import { useVirtualizer } from '@tanstack/vue-virtual'
import Icon from './Icon.vue'
import { useSettingsStore } from '@/stores/settings'

/**
 * 虚拟化数据表。
 *
 * `<table>` 的行没法绝对定位，所以 .dt.is-virtual 把 table 换成 grid/flex 布局，
 * 列宽由这里统一下发到 th 与 td —— 消费方只提供单元格内容，不碰宽度，
 * 否则表头和行迟早对不齐。
 */
export interface Column {
  key: string
  label: string
  /** 固定宽度；不给就按 grow 平分剩余空间 */
  width?: number
  grow?: boolean
  align?: 'left' | 'right'
  sortable?: boolean
  /** 表头上的补充说明，例如"每 key 额外 2 条命令" */
  title?: string
}

const props = defineProps<{
  rows: readonly T[]
  columns: Column[]
  rowKey: (row: T, index: number) => string | number
  sortKey?: string | null
  sortDesc?: boolean
  selectedKey?: string | number | null
  /**
   * 打开最左侧的复选框列。
   *
   * 勾选与「哪一行是当前行」（`selectedKey`）是两套语义，别混：
   * 点行仍然是打开预览页，复选框独占选择。两者合一的话，
   * 想勾第五行会先开一个标签页。见 doc/frontend/key-browser.md「多选」。
   */
  selectable?: boolean
}>()

/** 勾中的 rowKey。只可能是已加载的行 —— 表格并不知道库里还有多少。 */
const selected = defineModel<Set<string>>('selected', { default: () => new Set<string>() })

const emit = defineEmits<{
  sort: [key: string]
  rowClick: [row: T, index: number]
  rowDblClick: [row: T, index: number]
  rowContextMenu: [row: T, index: number]
}>()

const settings = useSettingsStore()
const scroller = ref<HTMLElement | null>(null)

/* 与 tokens.css 的 --h-row 保持一致（紧凑 30 / 标准 34 / 宽松 40） */
const ROW_HEIGHT: Record<string, number> = { compact: 30, standard: 34, cozy: 40 }
const rowHeight = computed(() => ROW_HEIGHT[settings.state.density] ?? 34)

const virtualizer = useVirtualizer(
  computed(() => ({
    count: props.rows.length,
    getScrollElement: () => scroller.value,
    estimateSize: () => rowHeight.value,
    overscan: 10,
    getItemKey: (index: number) => props.rowKey(props.rows[index], index),
  })),
)

const totalSize = computed(() => virtualizer.value.getTotalSize())
const visible = computed(() =>
  virtualizer.value
    .getVirtualItems()
    .map((item) => ({ item, row: props.rows[item.index] }))
    .filter((entry) => entry.row !== undefined),
)

function cellStyle(column: Column) {
  return column.width
    ? { width: `${column.width}px`, flex: `0 0 ${column.width}px` }
    : { flex: `${column.grow === false ? 0 : 1} 1 0`, minWidth: 0 }
}

/* ── 勾选 ─────────────────────────────────────────────── */

const keys = computed(() => props.rows.map((row, index) => String(props.rowKey(row, index))))
const allChecked = computed(() => keys.value.length > 0 && keys.value.every((key) => selected.value.has(key)))
const someChecked = computed(() => !allChecked.value && keys.value.some((key) => selected.value.has(key)))

function isChecked(row: T, index: number): boolean {
  return selected.value.has(String(props.rowKey(row, index)))
}

/** Set 是引用类型，改完要换一个新的才会触发更新。 */
function commit(next: Set<string>) {
  selected.value = next
}

/**
 * 表头复选框：全选/全不选**已加载的行**。
 *
 * SCAN 是增量的，表格里有 500 行不代表库里只有 500 个匹配 ——
 * 想对「全部匹配」动手要切到模式作用范围。界面上写明了这一点。
 */
function toggleAll() {
  if (allChecked.value) {
    const next = new Set(selected.value)
    keys.value.forEach((key) => next.delete(key))
    commit(next)
    return
  }
  commit(new Set([...selected.value, ...keys.value]))
}

/** 上一次勾选的位置，用来支持 Shift 范围选 —— 这是表格的通用期待。 */
let anchor: number | null = null

function toggleRow(row: T, index: number, event: MouseEvent) {
  const next = new Set(selected.value)
  const key = String(props.rowKey(row, index))

  if (event.shiftKey && anchor !== null) {
    const [from, to] = anchor <= index ? [anchor, index] : [index, anchor]
    // 范围一律按锚点那一步的方向走：勾就全勾、取消就全消
    const turningOn = !next.has(key)
    for (let cursor = from; cursor <= to; cursor += 1) {
      const rangeKey = keys.value[cursor]
      if (rangeKey === undefined) continue
      if (turningOn) next.add(rangeKey)
      else next.delete(rangeKey)
    }
  } else if (next.has(key)) {
    next.delete(key)
  } else {
    next.add(key)
  }

  anchor = index
  commit(next)
}

watch(
  () => props.rows.length,
  () => nextTick(() => virtualizer.value.measure()),
)

defineExpose({
  scrollToTop: () => virtualizer.value.scrollToIndex(0),
})
</script>

<template>
  <div ref="scroller" class="tablewrap">
    <table class="dt is-virtual">
      <thead>
        <tr>
          <th
            v-if="selectable"
            class="dt-check"
            title="全选/全不选已加载的行"
            @click="toggleAll"
          >
            <span
              class="cbx"
              :class="{ on: allChecked, 'is-part': someChecked }"
              role="checkbox"
              :aria-checked="allChecked ? 'true' : someChecked ? 'mixed' : 'false'"
            >
              <Icon v-if="allChecked" name="check" :size="12" />
              <Icon v-else-if="someChecked" name="minus" :size="12" />
            </span>
          </th>
          <th
            v-for="column in columns"
            :key="column.key"
            :class="{ r: column.align === 'right', sortable: column.sortable }"
            :style="cellStyle(column)"
            :title="column.title"
            @click="column.sortable && emit('sort', column.key)"
          >
            <span class="sorthead">
              {{ column.label }}
              <Icon
                v-if="column.sortable && sortKey === column.key"
                :name="sortDesc ? 'arrow-down' : 'arrow-up'"
                :size="12"
              />
            </span>
          </th>
        </tr>
      </thead>
      <tbody :style="{ height: `${totalSize}px` }">
        <tr
          v-for="{ item, row } in visible"
          :key="String(item.key)"
          :class="{ 'is-sel': selectedKey !== null && selectedKey === rowKey(row, item.index) }"
          :style="{ height: `${item.size}px`, transform: `translateY(${item.start}px)` }"
          @click="emit('rowClick', row, item.index)"
          @dblclick="emit('rowDblClick', row, item.index)"
          @contextmenu="emit('rowContextMenu', row, item.index)"
        >
          <!-- @click.stop：勾选不该顺带开一个标签页 -->
          <td
            v-if="selectable"
            class="dt-check"
            @click.stop="toggleRow(row, item.index, $event)"
          >
            <span
              class="cbx"
              :class="{ on: isChecked(row, item.index) }"
              role="checkbox"
              :aria-checked="isChecked(row, item.index)"
            >
              <Icon v-if="isChecked(row, item.index)" name="check" :size="12" />
            </span>
          </td>
          <td
            v-for="column in columns"
            :key="column.key"
            :class="{ r: column.align === 'right' }"
            :style="cellStyle(column)"
          >
            <slot :name="column.key" :row="row" :index="item.index" />
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>
