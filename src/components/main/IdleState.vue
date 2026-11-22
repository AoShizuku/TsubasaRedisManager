<script setup lang="ts">
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import { formatRelativeTime } from '@/lib/format'
import { useConnectionsStore } from '@/stores/connections'
import { useSessionsStore } from '@/stores/sessions'
import { useUiStore } from '@/stores/ui'

/** 未选中任何实例时的主区。对应原型 01-main-idle.html。 */
const connections = useConnectionsStore()
const sessions = useSessionsStore()
const ui = useUiStore()

async function open(key: string) {
  try {
    await sessions.openConnection(key)
  } catch (error) {
    ui.notifyError(error, '连接失败')
  }
}
</script>

<template>
  <div class="emptystate">
    <span class="emptystate-mark" />
    <h3>Tsubasa Redis Manager</h3>
    <p>从左侧选择一个连接，或新建一个。</p>

    <div v-if="connections.recentConnections.length" class="recent">
      <div class="cap">最近打开</div>
      <ul>
        <li v-for="entry in connections.recentConnections" :key="entry.connection.key">
          <button @click="open(entry.connection.key)">
            <span class="cdot" :style="{ background: entry.connection.color || 'var(--fg-3)' }" />
            <span class="clip">{{ entry.connection.name }}</span>
            <span class="muted mono clip">{{ entry.connection.host }}:{{ entry.connection.port }}</span>
            <span v-if="entry.connection.readonly" class="tag tag-danger">只读</span>
            <span class="when">{{ formatRelativeTime(entry.at) }}</span>
          </button>
        </li>
      </ul>
    </div>

    <div v-else-if="!connections.items.length" class="recent">
      <div class="cap">还没有连接</div>
      <div style="padding: 8px">
        <Btn variant="primary" icon="plus" @click="ui.openConnectionDialog()">新建连接</Btn>
      </div>
    </div>

    <div class="hintbar">
      <span><Icon name="plus" :size="12" /> 新建标签页</span>
      <span><span class="kbd">Ctrl B</span> Key 浏览器</span>
      <span><span class="kbd">Ctrl `</span> 命令行</span>
    </div>
  </div>
</template>
