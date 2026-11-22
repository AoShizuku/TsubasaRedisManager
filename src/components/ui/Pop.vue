<script setup lang="ts">
/**
 * 浮层。TTL 编辑这类"高频、低风险"的操作用它而不是模态 ——
 * 改个过期时间弹一层模态太重。
 */
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'reka-ui'

withDefaults(
  defineProps<{
    align?: 'start' | 'center' | 'end'
    side?: 'top' | 'right' | 'bottom' | 'left'
    width?: number
    sideOffset?: number
  }>(),
  { align: 'end', side: 'bottom', sideOffset: 6 },
)

const open = defineModel<boolean>('open', { default: false })
</script>

<template>
  <PopoverRoot v-model:open="open">
    <PopoverTrigger as-child>
      <slot name="trigger" />
    </PopoverTrigger>
    <PopoverPortal>
      <PopoverContent
        class="pop"
        :align="align"
        :side="side"
        :side-offset="sideOffset"
        :style="width ? { width: `${width}px` } : undefined"
      >
        <slot :close="() => (open = false)" />
      </PopoverContent>
    </PopoverPortal>
  </PopoverRoot>
</template>
