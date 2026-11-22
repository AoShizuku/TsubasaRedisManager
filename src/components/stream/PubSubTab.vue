<script setup lang="ts">
import { computed, ref } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Seg from '@/components/ui/Seg.vue'
import VirtualTable, { type Column } from '@/components/ui/VirtualTable.vue'
import { useTaskStream } from './useTaskStream'
import { startSubscription } from '@/api/redis'
import { formatCount } from '@/lib/format'
import { useTabsStore, type Tab } from '@/stores/tabs'
import { useUiStore } from '@/stores/ui'
import type { SubscriptionKind } from '@/types/ipc'

const props = defineProps<{ tab: Tab }>()
const tabs = useTabsStore()
const ui = useUiStore()

interface Message {
  id: number
  channel: string
  pattern: string | null
  message: string
}

let messageId = 0

const stream = useTaskStream<Message>({
  limit: 3000,
  map: (item) =>
    item.type === 'pubSubMessage'
      ? {
          id: (messageId += 1),
          channel: item.channel,
          pattern: item.pattern,
          message: item.message,
        }
      : null,
})

const kind = ref<SubscriptionKind>('subscribe')
const targets = ref('')

const kindOptions = [
  { value: 'subscribe', label: '频道' },
  { value: 'psubscribe', label: '模式' },
]

const columns: Column[] = [
  { key: 'channel', label: '频道', width: 220 },
  { key: 'pattern', label: '匹配模式', width: 160 },
  { key: 'message', label: '消息', grow: true },
]

const rows = computed(() => stream.rows.value)

function parseTargets(): string[] {
  return targets.value
    .split(/[\s,]+/)
    .map((value) => value.trim())
    .filter(Boolean)
}

async function subscribe() {
  const list = parseTargets()
  if (!list.length) {
    ui.notify('warn', kind.value === 'subscribe' ? '请填写频道名' : '请填写匹配模式')
    return
  }
  await stream.start(() =>
    startSubscription(props.tab.sessionId, {
      kind: kind.value,
      channels: kind.value === 'subscribe' ? list : [],
      patterns: kind.value === 'psubscribe' ? list : [],
    }),
  )
}
</script>

<template>
  <div class="toolbar">
    <Seg v-model="kind" :options="kindOptions" :class="{ 'is-hidden': stream.running.value }" />
    <input
      v-model="targets"
      class="input mono grow"
      style="max-width: 420px"
      :placeholder="kind === 'subscribe' ? 'news, orders:created' : 'orders:*'"
      :disabled="stream.running.value"
      @keydown.enter="subscribe"
    />
    <Btn v-if="!stream.running.value" size="sm" variant="primary" icon="bell" @click="subscribe">
      订阅
    </Btn>
    <Btn v-else size="sm" variant="danger" icon="ban" @click="stream.stop()">取消订阅</Btn>
    <Btn size="sm" icon="rotate-ccw" @click="stream.clear()">清空</Btn>

    <span class="spacer" />
    <span class="muted mono" style="font-size: var(--fs-11)">
      {{ tabs.scopeLabel(tab.sessionId) }}
    </span>
    <span class="muted num" style="font-size: var(--fs-11)">
      {{ formatCount(rows.length) }} 条
    </span>
  </div>

  <div v-if="!stream.running.value && !rows.length" class="emptyline">
    <span>填写频道或模式后订阅。订阅期间这个连接会被占用，无法执行其他命令。</span>
  </div>

  <VirtualTable
    v-else
    :rows="rows"
    :columns="columns"
    :row-key="(row) => row.id"
    :selected-key="null"
  >
    <template #channel="{ row }"><span class="k">{{ row.channel }}</span></template>
    <template #pattern="{ row }"><span class="v">{{ row.pattern ?? '—' }}</span></template>
    <template #message="{ row }"><span class="v">{{ row.message }}</span></template>
  </VirtualTable>
</template>
