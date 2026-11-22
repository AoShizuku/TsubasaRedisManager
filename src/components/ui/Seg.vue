<script setup lang="ts">
/** 分段控件。用于树形/列表这类互斥的视图切换。 */
import Icon from './Icon.vue'
import type { IconName } from '@/lib/icons'

export interface SegOption<T extends string> {
  value: T
  label: string
  icon?: IconName
  title?: string
}

defineProps<{ options: SegOption<string>[] }>()
const model = defineModel<string>({ required: true })
</script>

<template>
  <div class="seg" role="radiogroup">
    <button
      v-for="option in options"
      :key="option.value"
      type="button"
      role="radio"
      :aria-checked="model === option.value"
      :title="option.title"
      :class="{ 'is-active': model === option.value }"
      @click="model = option.value"
    >
      <Icon v-if="option.icon" :name="option.icon" :size="12" />
      {{ option.label }}
    </button>
  </div>
</template>
