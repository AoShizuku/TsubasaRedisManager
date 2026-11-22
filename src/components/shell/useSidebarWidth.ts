import { onBeforeUnmount, ref, watch } from 'vue'

const STORAGE_KEY = 'tsubasa.sidebar-width'
const MIN = 208
const MAX = 420

/** 侧栏宽度拖拽。范围 208–420，与 tokens.css 里 --w-side 的注释一致。 */
export function useSidebarWidth() {
  const stored = Number(localStorage.getItem(STORAGE_KEY))
  const width = ref(Number.isFinite(stored) && stored >= MIN ? Math.min(MAX, stored) : 268)
  const dragging = ref(false)

  let startX = 0
  let startWidth = 0

  function onMove(event: MouseEvent) {
    width.value = Math.min(MAX, Math.max(MIN, startWidth + event.clientX - startX))
  }

  function onUp() {
    dragging.value = false
    window.removeEventListener('mousemove', onMove)
    window.removeEventListener('mouseup', onUp)
    // 拖拽期间禁掉文本选择，松手后恢复
    document.body.style.removeProperty('cursor')
  }

  function start(event: MouseEvent) {
    if (event.button !== 0) return
    event.preventDefault()
    dragging.value = true
    startX = event.clientX
    startWidth = width.value
    document.body.style.setProperty('cursor', 'col-resize')
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup', onUp)
  }

  watch(width, (value) => localStorage.setItem(STORAGE_KEY, String(value)))
  onBeforeUnmount(onUp)

  return { width, dragging, start }
}
