<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import type { UnlistenFn } from '@tauri-apps/api/event'
import { hideWindow, minimizeWindow, toggleMaximize } from '@/api/native'
import Icon from '@/components/ui/Icon.vue'
import { useUiStore } from '@/stores/ui'
// 应用图标源稿；与安装包图标同一份，改图标只改这一个文件（生成步骤见文件内注释）
import appIcon from '../../../src-tauri/icons/icon.svg'

/**
 * 自绘标题栏。窗口的 decorations 关掉了，所以拖动、最大化、关闭全靠这里。
 * 拖动用 data-tauri-drag-region（同时提供双击最大化），三个按钮走后端命令。
 */
const ui = useUiStore()
const maximized = defineModel<boolean>('maximized', { default: false })

let unlisten: UnlistenFn | null = null
const win = ref<ReturnType<typeof getCurrentWindow> | null>(null)

async function sync() {
  try {
    maximized.value = await (win.value ?? getCurrentWindow()).isMaximized()
  } catch {
    // 浏览器里跑 vite dev 时没有 Tauri 运行时，忽略即可
  }
}

onMounted(async () => {
  try {
    win.value = getCurrentWindow()
    await sync()
    unlisten = await win.value.onResized(() => void sync())
  } catch {
    /* 同上 */
  }
})

onBeforeUnmount(() => unlisten?.())

async function onToggleMaximize() {
  await toggleMaximize()
  await sync()
}
</script>

<template>
  <div class="titlebar" data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <img class="brand-icon" :src="appIcon" alt="" draggable="false" />
      <span>Tsubasa Redis Manager</span>
    </div>

    <!-- 命令面板是 v0.1 之后的功能，这里先占住入口与快捷键位置 -->
    <button class="omnibar" title="命令面板将在后续版本提供" disabled>
      <Icon name="search" :size="12" />
      <span>搜索命令、连接与 Key</span>
      <span class="kbd">Ctrl ⇧ P</span>
    </button>

    <div class="wctl">
      <button title="设置" @click="ui.openSettings()">
        <Icon name="settings" :size="16" />
      </button>
      <button title="最小化" @click="minimizeWindow()">
        <Icon name="minus" :size="16" />
      </button>
      <button :title="maximized ? '向下还原' : '最大化'" @click="onToggleMaximize">
        <Icon :name="maximized ? 'copy' : 'square'" :size="12" />
      </button>
      <button class="close" title="关闭" @click="hideWindow()">
        <Icon name="x" :size="16" />
      </button>
    </div>
  </div>
</template>
