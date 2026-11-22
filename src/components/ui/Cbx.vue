<script setup lang="ts">
import { CheckboxRoot } from 'reka-ui'
import Icon from './Icon.vue'

defineProps<{ disabled?: boolean }>()
const model = defineModel<boolean>({ default: false })
</script>

<template>
  <CheckboxRoot v-model="model" :disabled="disabled" class="cbx" :class="{ on: model }">
    <Icon v-if="model" name="check" :size="12" />
  </CheckboxRoot>
</template>
