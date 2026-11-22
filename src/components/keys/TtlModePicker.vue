<script setup lang="ts">
import Icon from '@/components/ui/Icon.vue'
import { TTL_PRESETS, TTL_UNITS, type TtlDraft } from '@/lib/ttl'

/**
 * 三模式 + 单位 + 快捷预设的那一组控件。
 *
 * 从 `TtlChip.vue` 抽出来给批量对话框共用 —— 三种模式的语义在单 key 和批量
 * 之间必须是同一套（doc/frontend/batch.md「批量不是 for 循环」），两边各画一遍迟早会漂开。
 * 换算与校验在 `useTtlDraft`，这里只管 DOM。
 */
const draft = defineModel<TtlDraft>({ required: true })
defineProps<{ hideCommandHints?: boolean }>()

function applyPreset(seconds: number) {
  draft.value = { ...draft.value, mode: 'relative', amount: seconds, unit: 1 }
}

function setMode(mode: TtlDraft['mode']) {
  draft.value = { ...draft.value, mode }
}
</script>

<template>
  <label class="pop-opt">
    <button type="button" class="radio" :class="{ on: draft.mode === 'persist' }" :aria-pressed="draft.mode === 'persist'" aria-label="不过期" @click="setMode('persist')" />
    <span>不过期</span>
    <span class="spacer" />
    <span v-if="!hideCommandHints" class="muted mono" style="font-size: var(--fs-10)">PERSIST</span>
  </label>

  <label class="pop-opt">
    <button type="button" class="radio" :class="{ on: draft.mode === 'relative' }" :aria-pressed="draft.mode === 'relative'" aria-label="相对过期时间" @click="setMode('relative')" />
    <span>相对</span>
    <input
      v-model.number="draft.amount"
      class="input w-88 num"
      type="number"
      min="1"
      @focus="setMode('relative')"
    />
    <!-- 单位下拉避免心算，内部一律存秒 -->
    <div class="select">
      <select v-model.number="draft.unit" class="input w-64" @focus="setMode('relative')">
        <option v-for="option in TTL_UNITS" :key="option.value" :value="option.value">
          {{ option.label }}
        </option>
      </select>
      <span class="caret"><Icon name="chevron-down" :size="12" /></span>
    </div>
  </label>

  <label class="pop-opt">
    <button type="button" class="radio" :class="{ on: draft.mode === 'absolute' }" :aria-pressed="draft.mode === 'absolute'" aria-label="绝对过期时间" @click="setMode('absolute')" />
    <span>绝对</span>
    <input
      v-model="draft.absolute"
      class="input grow num"
      type="datetime-local"
      @focus="setMode('absolute')"
    />
  </label>

  <div class="pop-opt">
    <span class="muted" style="font-size: var(--fs-11)">快捷</span>
    <button
      v-for="preset in TTL_PRESETS"
      :key="preset.seconds"
      class="chipbtn"
      type="button"
      @click="applyPreset(preset.seconds)"
    >
      {{ preset.label }}
    </button>
  </div>
</template>
