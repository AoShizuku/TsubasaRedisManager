<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { TooltipProvider } from 'reka-ui'
import ResizeHandles from '@/components/shell/ResizeHandles.vue'
import TitleBar from '@/components/shell/TitleBar.vue'
import TabBar from '@/components/shell/TabBar.vue'
import StatusBar from '@/components/shell/StatusBar.vue'
import Sidebar from '@/components/sidebar/Sidebar.vue'
import MainArea from '@/components/main/MainArea.vue'
import BatchDeleteDialog from '@/components/dialogs/BatchDeleteDialog.vue'
import BatchTtlDialog from '@/components/dialogs/BatchTtlDialog.vue'
import KeyWriteDialog from '@/components/dialogs/KeyWriteDialog.vue'
import ConnectionDialog from '@/components/dialogs/ConnectionDialog.vue'
import SettingsDialog from '@/components/dialogs/SettingsDialog.vue'
import ConfirmDialog from '@/components/ui/ConfirmDialog.vue'
import PromptDialog from '@/components/ui/PromptDialog.vue'
import Toasts from '@/components/ui/Toasts.vue'
import { useSidebarWidth } from '@/components/shell/useSidebarWidth'
import { useShortcuts } from '@/components/shell/useShortcuts'
import { loadMonaco } from '@/lib/monacoLoader'
import { onCapabilityChange, onConnectionStatus, type UnlistenFn } from '@/api/events'
import { useConnectionsStore } from '@/stores/connections'
import { useSessionsStore } from '@/stores/sessions'
import { useTasksStore } from '@/stores/tasks'
import { useUiStore } from '@/stores/ui'

const connections = useConnectionsStore()
const sessions = useSessionsStore()
const tasks = useTasksStore()
const ui = useUiStore()

const maximized = ref(false)
const { width, dragging, start } = useSidebarWidth()
useShortcuts()

let unlistenStatus: UnlistenFn | null = null
let unlistenCapability: UnlistenFn | null = null

/**
 * 空闲时把 Monaco 拉起来。
 *
 * 它近 4 MB，等第一个 String 打开时才下载解析就是肉眼可见的一段等待。而它是
 * **应用级**资源不是会话级的，所以不挂在会话或扫描结果上——挑「首批扫描里有
 * String」之类的条件，收益接近零（几乎每个库都有 String），却要让 store 知道
 * 编辑器的存在。真正打开时 `loadMonaco` 复用同一个 promise。
 *
 * 失败要吞掉：预热失败不该冒成未捕获的 rejection，真去打开编辑器时还会再报一次。
 */
function prewarmMonaco() {
  const schedule: (run: () => void) => void =
    typeof window.requestIdleCallback === 'function'
      ? (run) => void window.requestIdleCallback(run)
      : (run) => void window.setTimeout(run, 200)
  schedule(() => void loadMonaco().catch(() => {}))
}

onMounted(async () => {
  prewarmMonaco()
  await connections.refresh()
  try {
    await tasks.start()
    unlistenStatus = await onConnectionStatus((event) => {
      if (event.status !== 'failed') return
      ui.notify('danger', `${event.connectionName} 连接失败`, event.message)
    })
    // 失败本身由触发它的那次操作报（redis.capability_unsupported），这里只让界面随之置灰
    unlistenCapability = await onCapabilityChange((event) => sessions.applyCapabilityChange(event))
  } catch {
    // 没有 Tauri 运行时（例如直接用 vite dev 在浏览器里看样式）时事件订阅会失败，
    // 界面本身照常可用，不该因此整个挂掉
  }
})

onBeforeUnmount(() => {
  unlistenStatus?.()
  unlistenCapability?.()
  void tasks.stop()
})
</script>

<template>
  <TooltipProvider :delay-duration="400">
    <div class="win" :class="{ 'is-maximized': maximized }">
      <ResizeHandles />
      <TitleBar v-model:maximized="maximized" />

      <div class="win-body">
        <Sidebar :style="{ '--w-side': `${width}px` }" />
        <div class="side-split" :class="{ 'is-dragging': dragging }" @mousedown="start" />

        <main class="main">
          <TabBar />
          <MainArea />
        </main>
      </div>

      <StatusBar />
    </div>

    <ConnectionDialog />
    <SettingsDialog />
    <BatchTtlDialog />
    <KeyWriteDialog />
    <BatchDeleteDialog />
    <ConfirmDialog />
    <PromptDialog />
    <Toasts />
  </TooltipProvider>
</template>
