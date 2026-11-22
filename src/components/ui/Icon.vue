<script setup lang="ts">
import { computed } from 'vue'
import { ICONS, strokeFor, type IconName } from '@/lib/icons'

const props = withDefaults(defineProps<{ name: IconName; size?: 12 | 16 | 20 }>(), {
  size: 16,
})

const component = computed(() => ICONS[props.name])
const stroke = computed(() => strokeFor(props.size))
</script>

<template>
  <!-- loader 一律自转：它在六个地方被当作"正在忙"的指示，
       不转的话和普通图标没有区别，等同于没有反馈。 -->
  <component
    :is="component"
    class="i"
    :class="{ 'i-12': size === 12, 'i-20': size === 20, 'i-spin': name === 'loader' }"
    :size="size"
    :stroke-width="stroke"
    aria-hidden="true"
  />
</template>
