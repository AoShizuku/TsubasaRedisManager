<script setup lang="ts">
import { useUiStore } from '@/stores/ui'
import Icon from './Icon.vue'
import type { IconName } from '@/lib/icons'
import type { ToastKind } from '@/stores/ui'

const ui = useUiStore()

const ICON: Record<ToastKind, IconName> = {
  ok: 'circle-check',
  warn: 'alert-triangle',
  danger: 'alert-circle',
  info: 'info',
}
</script>

<template>
  <div class="toasts" role="status" aria-live="polite">
    <div v-for="toast in ui.toasts" :key="toast.id" class="toast" :class="`banner-${toast.kind}`">
      <Icon :name="ICON[toast.kind]" :size="16" />
      <div class="grow">
        <div>{{ toast.text }}</div>
        <div v-if="toast.detail" class="toast-detail selectable">{{ toast.detail }}</div>
      </div>
      <button class="btn btn-ghost btn-icon btn-sm" title="关闭" @click="ui.dismiss(toast.id)">
        <Icon name="x" :size="12" />
      </button>
    </div>
  </div>
</template>
