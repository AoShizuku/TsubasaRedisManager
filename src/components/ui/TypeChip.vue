<script setup lang="ts">
import { computed } from 'vue'
import { keyTypeClass, keyTypeMeta } from '@/lib/keyTypes'

const props = defineProps<{ type: string | null | undefined }>()
const cls = computed(() => keyTypeClass(props.type))
const label = computed(() => keyTypeMeta(props.type).label)
</script>

<template>
  <span class="chip" :class="cls">{{ label }}</span>
</template>
