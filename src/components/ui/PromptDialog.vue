<script setup lang="ts">
import { computed } from 'vue'
import Btn from './Btn.vue'
import Dlg from './Dlg.vue'
import { useUiStore } from '@/stores/ui'

const ui = useUiStore()

const open = computed({
  get: () => !!ui.promptState?.open,
  set: (value: boolean) => {
    if (!value) ui.resolvePrompt(null)
  },
})

function submit() {
  const state = ui.promptState
  if (!state) return
  const result: Record<string, string> = {}
  for (const field of state.fields) result[field.name] = field.value
  ui.resolvePrompt(result)
}
</script>

<template>
  <Dlg v-if="ui.promptState" v-model:open="open" :title="ui.promptState.title" :width="520">
    <div class="form">
      <div
        v-for="field in ui.promptState.fields"
        :key="field.name"
        class="field"
        :style="field.multiline ? 'align-items: start' : undefined"
      >
        <label>{{ field.label }}</label>
        <div class="field-ctl">
          <textarea
            v-if="field.multiline"
            v-model="field.value"
            class="input mono grow"
            rows="8"
          />
          <input
            v-else
            v-model="field.value"
            class="input"
            :class="{ mono: field.mono }"
            @keydown.enter="submit"
          />
        </div>
      </div>
    </div>
    <template #footer>
      <span class="spacer" />
      <Btn @click="ui.resolvePrompt(null)">取消</Btn>
      <Btn variant="primary" @click="submit">{{ ui.promptState.confirmText }}</Btn>
    </template>
  </Dlg>
</template>
