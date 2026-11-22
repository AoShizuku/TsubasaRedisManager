<script setup lang="ts">
import { computed } from 'vue'
import Btn from './Btn.vue'
import Dlg from './Dlg.vue'
import { useUiStore } from '@/stores/ui'

const ui = useUiStore()

const open = computed({
  get: () => !!ui.confirmState?.open,
  // 点遮罩、按 ESC、点右上角关闭都算取消
  set: (value: boolean) => {
    if (!value) ui.resolveConfirm(false)
  },
})
</script>

<template>
  <Dlg v-if="ui.confirmState" v-model:open="open" :title="ui.confirmState.title" :width="420">
    <div class="form">
      <p style="font-size: var(--fs-12)">{{ ui.confirmState.body }}</p>
      <p v-if="ui.confirmState.detail" class="mono selectable" style="margin-top: 8px; font-size: var(--fs-11); color: var(--fg-3); word-break: break-all">
        {{ ui.confirmState.detail }}
      </p>
    </div>
    <template #footer>
      <span class="spacer" />
      <Btn @click="ui.resolveConfirm(false)">取消</Btn>
      <Btn
        :variant="ui.confirmState.danger ? 'dangerSolid' : 'primary'"
        @click="ui.resolveConfirm(true)"
      >
        {{ ui.confirmState.confirmText }}
      </Btn>
    </template>
  </Dlg>
</template>
