<script setup lang="ts">
/**
 * 对话框外壳。外观自绘，焦点陷阱 / ESC 关闭 / 滚动锁定 / aria 交给 reka-ui。
 * 这几件事自己写十有八九写不对，是引入 reka-ui 的主要理由。
 */
import { DialogContent, DialogDescription, DialogOverlay, DialogPortal, DialogRoot, DialogTitle } from 'reka-ui'
import Icon from './Icon.vue'

withDefaults(
  defineProps<{
    title: string
    description?: string
    width?: number
    /** 正在提交时禁止点遮罩关闭，避免半途中断写操作 */
    busy?: boolean
  }>(),
  { width: 560 },
)

const open = defineModel<boolean>('open', { required: true })
</script>

<template>
  <DialogRoot v-model:open="open">
    <DialogPortal>
      <DialogOverlay class="scrim" />
      <DialogContent
        class="dlg"
        :style="{ width: `${width}px` }"
        @interact-outside="busy && $event.preventDefault()"
        @escape-key-down="busy && $event.preventDefault()"
      >
        <div class="dlg-hd">
          <div class="grow">
            <DialogTitle as="h3">{{ title }}</DialogTitle>
            <DialogDescription v-if="description" as="p">{{ description }}</DialogDescription>
          </div>
          <button class="btn btn-ghost btn-icon btn-sm" title="关闭" :disabled="busy" @click="open = false">
            <Icon name="x" :size="16" />
          </button>
        </div>

        <slot name="tabs" />

        <div class="dlg-body">
          <slot />
        </div>

        <div v-if="$slots.footer" class="dlg-ft">
          <slot name="footer" />
        </div>
      </DialogContent>
    </DialogPortal>
  </DialogRoot>
</template>
