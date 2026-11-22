<script setup lang="ts">
/**
 * 菜单项。下拉与右键菜单在 reka-ui 里是两套组件（共享内部 Menu 原语，
 * 但 MenuItem 本身没有公开导出），所以由外层通过 provide 告知风味，
 * 这里再挑对应的实现 —— 避免为同一个视觉写两遍。
 */
import { computed, inject } from 'vue'
import { ContextMenuItem, DropdownMenuItem } from 'reka-ui'
import Icon from './Icon.vue'
import type { IconName } from '@/lib/icons'
import { MENU_FLAVOR, type MenuFlavor } from './menuFlavor'

const props = defineProps<{
  icon?: IconName
  shortcut?: string
  danger?: boolean
  disabled?: boolean
  /** 置灰时的解释，例如"该连接为只读模式" */
  title?: string
  trail?: string
}>()

const emit = defineEmits<{ select: [] }>()

const flavor = inject<MenuFlavor>(MENU_FLAVOR, 'dropdown')
const component = computed(() => (flavor === 'context' ? ContextMenuItem : DropdownMenuItem))
</script>

<template>
  <component
    :is="component"
    class="menu-item"
    :class="{ danger: props.danger }"
    :disabled="props.disabled"
    :title="props.title"
    @select="emit('select')"
  >
    <Icon v-if="props.icon" :name="props.icon" :size="16" />
    <slot />
    <span v-if="props.trail" class="mi-trail">{{ props.trail }}</span>
    <span v-if="props.shortcut" class="kbd">{{ props.shortcut }}</span>
  </component>
</template>
