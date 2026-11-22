<script setup lang="ts">
import { computed } from 'vue'
import IdleState from './IdleState.vue'
import KeyTab from '@/components/keys/KeyTab.vue'
import KeyBrowserTab from '@/components/browser/KeyBrowserTab.vue'
import CliTab from '@/components/cli/CliTab.vue'
import MonitorTab from '@/components/stream/MonitorTab.vue'
import PubSubTab from '@/components/stream/PubSubTab.vue'
import MemoryTab from '@/components/stream/MemoryTab.vue'
import { useSessionsStore } from '@/stores/sessions'
import { useTabsStore } from '@/stores/tabs'

const tabs = useTabsStore()
const sessions = useSessionsStore()

const tab = computed(() => tabs.active)
/** 会话可能在标签页还开着的时候被关掉；此时不要继续渲染一个连不上的面板。 */
const alive = computed(() => (tab.value ? sessions.sessions.has(tab.value.sessionId) : false))

/**
 * 渲染 key 必须带上内容，不能只用 tab.id。
 * 预览页是**就地换内容**的（id 不变，key 变），只按 id 建 key 的话
 * 组件不会重挂载，会拿着上一个 key 的编辑器状态继续显示。
 */
const renderKey = computed(() =>
  tab.value ? `${tab.value.id}:${tab.value.sessionId}:${tab.value.key ?? ''}` : '',
)
</script>

<template>
  <IdleState v-if="!tab" />

  <div v-else-if="!alive" class="emptystate">
    <h3>连接已关闭</h3>
    <p>这个标签页所属的会话已经断开。</p>
    <div style="margin-top: 16px">
      <button class="btn" @click="tabs.close(tab.id)">关闭标签页</button>
    </div>
  </div>

  <KeyTab v-else-if="tab.kind === 'key'" :key="renderKey" :tab="tab" />
  <KeyBrowserTab v-else-if="tab.kind === 'browser'" :key="renderKey" :tab="tab" />
  <CliTab v-else-if="tab.kind === 'cli'" :key="renderKey" :tab="tab" />
  <MonitorTab v-else-if="tab.kind === 'monitor'" :key="renderKey" :tab="tab" />
  <PubSubTab v-else-if="tab.kind === 'pubsub'" :key="renderKey" :tab="tab" />
  <MemoryTab v-else :key="renderKey" :tab="tab" />
</template>
