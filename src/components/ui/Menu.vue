<script setup lang="ts">
/**
 * 下拉菜单。触发器放进 #trigger 插槽，菜单项放进默认插槽。
 * 键盘漫游、typeahead、碰撞翻转都由 reka-ui 处理。
 */
import { provide } from 'vue'
import { DropdownMenuContent, DropdownMenuPortal, DropdownMenuRoot, DropdownMenuTrigger } from 'reka-ui'
import { MENU_FLAVOR } from './menuFlavor'

provide(MENU_FLAVOR, 'dropdown')

withDefaults(
  defineProps<{
    align?: 'start' | 'center' | 'end'
    side?: 'top' | 'right' | 'bottom' | 'left'
    width?: number
    sideOffset?: number
  }>(),
  { align: 'start', side: 'bottom', sideOffset: 4 },
)

const open = defineModel<boolean>('open', { default: false })
</script>

<template>
  <DropdownMenuRoot v-model:open="open">
    <DropdownMenuTrigger as-child>
      <slot name="trigger" />
    </DropdownMenuTrigger>
    <DropdownMenuPortal>
      <DropdownMenuContent
        class="menu"
        :align="align"
        :side="side"
        :side-offset="sideOffset"
        :style="width ? { width: `${width}px` } : undefined"
      >
        <slot />
      </DropdownMenuContent>
    </DropdownMenuPortal>
  </DropdownMenuRoot>
</template>
