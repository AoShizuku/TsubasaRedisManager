<script setup lang="ts">
/**
 * 右键菜单。#trigger 插槽里的元素整块可右键。
 *
 * 树里的紧凑合并行需要**分段**右键：每一段各自包一个 CtxMenu，
 * 而不是整行包一个 —— 否则中间层级就没有操作入口了。
 */
import { provide } from 'vue'
import { ContextMenuContent, ContextMenuPortal, ContextMenuRoot, ContextMenuTrigger } from 'reka-ui'
import { MENU_FLAVOR } from './menuFlavor'

provide(MENU_FLAVOR, 'context')

defineProps<{ width?: number; disabled?: boolean }>()
const open = defineModel<boolean>('open', { default: false })
</script>

<template>
  <ContextMenuRoot v-model:open="open">
    <ContextMenuTrigger as-child :disabled="disabled">
      <slot name="trigger" />
    </ContextMenuTrigger>
    <ContextMenuPortal>
      <ContextMenuContent class="menu" :style="width ? { width: `${width}px` } : undefined">
        <slot />
      </ContextMenuContent>
    </ContextMenuPortal>
  </ContextMenuRoot>
</template>
