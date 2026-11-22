import { defineStore } from 'pinia'
import { reactive, ref, watch } from 'vue'
import { setZoomFactor } from '@/api/native'

export type Density = 'compact' | 'standard' | 'cozy'
export type TreeMode = 'tree' | 'list'

/**
 * 用户设置。全部落 localStorage —— 这些是界面偏好，不该占用后端的
 * connections.json，也不该跟着连接配置走。
 */
export interface Settings {
  /* 外观 */
  density: Density
  uiFont: string
  monoFont: string
  zoom: number
  /* Key 树 */
  compactPaths: boolean
  indentGuides: boolean
  /** 树里每个 key 显示 TTL 意味着每 key 多一条 TTL 命令，默认关闭。 */
  treeTtl: boolean
  defaultTreeMode: TreeMode
  /* 扫描 */
  scanCount: number
  /**
   * 扫到底任务（搜索 / 前缀扫描）的每批 COUNT，独立于浏览路径的 scanCount：
   * 任务带 MATCH 时命中稀疏、回包小，大 COUNT 纯粹省往返，默认 2000
   * （doc/backend/tasks.md「扫到底的扫描任务」，这个默认值是对的，不要调低）。
   */
  taskScanCount: number
  treeNodeLimit: number
  /* 行为 */
  ttlSoonThreshold: number
  confirmDelete: boolean
  /** Key 浏览器的分析模式：每 key 额外 2 条命令，默认关闭。 */
  analysisMode: boolean
  /* 编辑器 */
  editorWordWrap: boolean
  editorMinimap: boolean
  editorTabSize: number
  formatJsonOnOpen: boolean
  /* 格式化器 */
  formatterCommand: string
  formatterParams: string
  formatterTimeoutMs: number
  formatterMaxOutputBytes: number
  /* 更新 */
  updateEndpoint: string
}

const DEFAULTS: Settings = {
  density: 'standard',
  uiFont: '',
  monoFont: '',
  zoom: 1,
  compactPaths: true,
  indentGuides: true,
  treeTtl: false,
  defaultTreeMode: 'tree',
  scanCount: 500,
  taskScanCount: 2000,
  treeNodeLimit: 20000,
  ttlSoonThreshold: 60,
  confirmDelete: true,
  analysisMode: false,
  editorWordWrap: true,
  editorMinimap: false,
  editorTabSize: 2,
  formatJsonOnOpen: true,
  formatterCommand: '',
  formatterParams: '',
  formatterTimeoutMs: 5000,
  formatterMaxOutputBytes: 2 * 1024 * 1024,
  updateEndpoint: '',
}

const STORAGE_KEY = 'tsubasa.settings.v1'

function load(): Settings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return { ...DEFAULTS }
    const parsed = JSON.parse(raw) as Partial<Settings>
    // 逐字段合并而不是整体替换：加了新设置项时老用户不会拿到 undefined
    return { ...DEFAULTS, ...parsed }
  } catch {
    return { ...DEFAULTS }
  }
}

export const useSettingsStore = defineStore('settings', () => {
  const state = reactive<Settings>(load())
  const fontCandidates = ref<string[]>([])

  /** 密度与字体是 CSS 变量驱动的，改动直接写到根元素上。 */
  function applyToDocument() {
    const root = document.documentElement
    root.dataset.density = state.density
    root.style.setProperty('--ff-ui-user', state.uiFont ? `"${state.uiFont}", var(--ff-ui)` : 'var(--ff-ui)')
    root.style.setProperty(
      '--ff-mono-user',
      state.monoFont ? `"${state.monoFont}", var(--ff-mono)` : 'var(--ff-mono)',
    )
  }

  watch(
    () => ({ ...state }),
    (value) => {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(value))
      applyToDocument()
    },
    { deep: true },
  )

  applyToDocument()

  async function setZoom(value: number) {
    const clamped = Math.min(2, Math.max(0.6, Number(value.toFixed(2))))
    state.zoom = clamped
    // 后端只做校验并回显，实际缩放由 webview 承担
    await setZoomFactor(clamped).catch(() => undefined)
    document.documentElement.style.setProperty('zoom', String(clamped))
  }

  function reset() {
    Object.assign(state, DEFAULTS)
  }

  return { state, fontCandidates, setZoom, reset, applyToDocument }
})
