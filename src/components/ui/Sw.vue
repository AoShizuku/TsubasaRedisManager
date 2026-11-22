<script setup lang="ts">
/**
 * 开关。外观是自绘的（.sw 的伪元素滑块），行为交给 reka-ui：
 * 它负责 role="switch"、aria-checked、空格/回车键与表单联动。
 */
import { SwitchRoot } from 'reka-ui'

withDefaults(defineProps<{ disabled?: boolean; size?: 'md' | 'sm' }>(), { size: 'md' })
const model = defineModel<boolean>({ default: false })
</script>

<template>
  <SwitchRoot
    v-model="model"
    :disabled="disabled"
    class="sw"
    :class="{ on: model, sm: size === 'sm' }"
  />
</template>
