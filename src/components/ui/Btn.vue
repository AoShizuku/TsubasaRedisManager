<script setup lang="ts">
import { computed } from 'vue'
import Icon from './Icon.vue'
import type { IconName } from '@/lib/icons'

const props = withDefaults(
  defineProps<{
    variant?: 'default' | 'primary' | 'danger' | 'dangerSolid' | 'ghost'
    size?: 'md' | 'sm'
    icon?: IconName
    /** 只有图标、没有文字时用；务必同时给 title 或 aria-label */
    iconOnly?: boolean
    active?: boolean
    type?: 'button' | 'submit'
  }>(),
  { variant: 'default', size: 'md', type: 'button' },
)

const classes = computed(() => [
  'btn',
  {
    'btn-primary': props.variant === 'primary',
    'btn-danger': props.variant === 'danger',
    'btn-danger-solid': props.variant === 'dangerSolid',
    'btn-ghost': props.variant === 'ghost',
    'btn-sm': props.size === 'sm',
    'btn-icon': props.iconOnly,
    'is-on': props.active,
  },
])

const iconSize = computed<12 | 16>(() => (props.size === 'sm' ? 12 : 16))
</script>

<template>
  <button :type="type" :class="classes">
    <Icon v-if="icon" :name="icon" :size="iconSize" />
    <slot />
  </button>
</template>
