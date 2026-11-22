<script setup lang="ts">
/**
 * 无边框窗口的缩放热区。
 * decorations: false 会连系统的缩放边框一起去掉，必须自己补，
 * 否则窗口只能通过最大化改变尺寸。
 */
import { getCurrentWindow } from '@tauri-apps/api/window'

type Direction =
  | 'North'
  | 'South'
  | 'East'
  | 'West'
  | 'NorthEast'
  | 'NorthWest'
  | 'SouthEast'
  | 'SouthWest'

const HANDLES: Array<{ cls: string; dir: Direction }> = [
  { cls: 'rsz-n', dir: 'North' },
  { cls: 'rsz-s', dir: 'South' },
  { cls: 'rsz-w', dir: 'West' },
  { cls: 'rsz-e', dir: 'East' },
  { cls: 'rsz-nw', dir: 'NorthWest' },
  { cls: 'rsz-ne', dir: 'NorthEast' },
  { cls: 'rsz-sw', dir: 'SouthWest' },
  { cls: 'rsz-se', dir: 'SouthEast' },
]

async function start(direction: Direction, event: MouseEvent) {
  if (event.button !== 0) return
  try {
    await getCurrentWindow().startResizeDragging(direction)
  } catch {
    // 非 Tauri 环境下无操作
  }
}
</script>

<template>
  <div
    v-for="handle in HANDLES"
    :key="handle.cls"
    class="rsz"
    :class="handle.cls"
    @mousedown="start(handle.dir, $event)"
  />
</template>
