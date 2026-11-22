<script setup lang="ts">
import { TooltipContent, TooltipPortal, TooltipRoot, TooltipTrigger } from 'reka-ui'

withDefaults(
  defineProps<{ text: string; side?: 'top' | 'right' | 'bottom' | 'left'; delay?: number }>(),
  { side: 'bottom', delay: 400 },
)
</script>

<template>
  <TooltipRoot :delay-duration="delay">
    <TooltipTrigger as-child>
      <slot />
    </TooltipTrigger>
    <TooltipPortal>
      <TooltipContent class="tip" :side="side" :side-offset="6">{{ text }}</TooltipContent>
    </TooltipPortal>
  </TooltipRoot>
</template>
