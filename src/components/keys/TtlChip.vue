<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import Pop from '@/components/ui/Pop.vue'
import TtlModePicker from './TtlModePicker.vue'
import { setKeyTtl } from '@/api/redis'
import { formatDateTime, formatTtl, ttlState } from '@/lib/format'
import { newTtlDraft, ttlCommandFor, useTtlCountdown, useTtlDraft } from '@/lib/ttl'
import { useSettingsStore } from '@/stores/settings'
import { useUiStore } from '@/stores/ui'

/**
 * TTL 既是读数也是入口，所以它是控件而不是元数据 ——
 * 它是元信息里唯一会自己变化、唯一自带破坏性、唯一被高频修改的一项。
 * 详见 doc/frontend/ttl.md「TTL 是控件，不是元数据」。
 */
const props = defineProps<{
  sessionId: string
  keyName: string
  ttl: number | null
  readonly: boolean
}>()

const emit = defineEmits<{ changed: [ttl: number] }>()

const settings = useSettingsStore()
const ui = useUiStore()

const source = computed(() => props.ttl)
const { remaining } = useTtlCountdown(source)

const state = computed(() =>
  remaining.value === null ? 'persist' : ttlState(remaining.value, settings.state.ttlSoonThreshold),
)
const text = computed(() => (remaining.value === null ? '—' : formatTtl(remaining.value)))

/* ── 编辑 ─────────────────────────────────────────────── */

const open = ref(false)
const draft = ref(newTtlDraft(null))
const busy = ref(false)

const { seconds, absoluteMs, resultAt, invalid, value } = useTtlDraft(draft)

/** 每次打开都按当前实际值重置，避免残留上一次的输入。 */
watch(open, (isOpen) => {
  if (isOpen) draft.value = newTtlDraft(remaining.value)
})

const preview = computed(
  () => ttlCommandFor(draft.value.mode, props.keyName, seconds.value, absoluteMs.value).preview,
)

async function apply(close: () => void) {
  if (invalid.value) return
  busy.value = true
  try {
    await setKeyTtl(props.sessionId, props.keyName, draft.value.mode, value.value)
    // 应用成功后立刻切到新值，不等下一次刷新
    emit(
      'changed',
      draft.value.mode === 'persist'
        ? -1
        : Math.floor((resultAt.value!.getTime() - Date.now()) / 1000),
    )
    ui.notify('ok', '过期时间已更新')
    close()
  } catch (error) {
    ui.notifyError(error, '设置过期时间失败')
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <!-- 只读连接下 chip 照常显示读数（读值本身有价值），只是点不开 -->
  <button
    v-if="readonly"
    class="ttl"
    :class="`is-${state}`"
    disabled
    title="该连接为只读模式，无法修改过期时间"
  >
    <Icon name="clock" :size="12" />
    <span class="val">{{ text }}</span>
  </button>

  <Pop v-else v-model:open="open" :width="288" align="end">
    <template #trigger>
      <button class="ttl" :class="`is-${state}`" title="点击修改过期时间">
        <Icon name="clock" :size="12" />
        <span class="val">{{ text }}</span>
        <Icon class="caret" name="chevron-down" :size="12" />
      </button>
    </template>

    <template #default="{ close }">
      <div class="pop-hd">过期时间</div>
      <div class="pop-body">
        <TtlModePicker v-model="draft" />
      </div>

      <div class="pop-note">
        <template v-if="invalid">
          <span style="color: var(--brand)">绝对时刻必须晚于当前时间</span>
        </template>
        <template v-else-if="resultAt">到期于 {{ formatDateTime(resultAt) }}</template>
        <template v-else>该 Key 将不再过期</template>
        <span class="cmd">{{ preview }}</span>
      </div>

      <div class="pop-ft">
        <span class="spacer" />
        <Btn size="sm" @click="close()">取消</Btn>
        <Btn size="sm" variant="primary" :disabled="busy || invalid" @click="apply(close)">
          应用
        </Btn>
      </div>
    </template>
  </Pop>
</template>
