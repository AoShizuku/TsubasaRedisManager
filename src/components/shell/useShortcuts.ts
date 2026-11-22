import { onBeforeUnmount, onMounted } from 'vue'
import { useSessionsStore } from '@/stores/sessions'
import { useTabsStore } from '@/stores/tabs'
import { useUiStore } from '@/stores/ui'

/** 与设置 → 快捷键那张表一一对应。改这里就要同步改那张表。 */
export function useShortcuts() {
  const sessions = useSessionsStore()
  const tabs = useTabsStore()
  const ui = useUiStore()

  /** 焦点在输入框里时不抢键：用户可能正在敲 Redis 命令。 */
  function isTyping(target: EventTarget | null): boolean {
    const element = target as HTMLElement | null
    if (!element) return false
    const tag = element.tagName
    return tag === 'INPUT' || tag === 'TEXTAREA' || element.isContentEditable
  }

  function scopeSessionId(): string | null {
    return sessions.selected?.sessionId ?? sessions.activeSessionId ?? null
  }

  function onKeydown(event: KeyboardEvent) {
    const typing = isTyping(event.target)

    if (event.ctrlKey && !event.shiftKey && !event.altKey) {
      switch (event.key.toLowerCase()) {
        case 'b': {
          if (typing) return
          const sessionId = scopeSessionId()
          if (!sessionId) return ui.notify('warn', '请先在左侧打开一个数据库')
          event.preventDefault()
          tabs.open({ kind: 'browser', sessionId })
          return
        }
        case '`': {
          const sessionId = scopeSessionId()
          if (!sessionId) return ui.notify('warn', '请先在左侧打开一个数据库')
          event.preventDefault()
          tabs.open({ kind: 'cli', sessionId })
          return
        }
        case 'w': {
          /*
           * 先拦下来再判要不要关：webview 对 Ctrl+W 的默认行为是关掉整个窗口，
           * 漏出去比误关一个标签页严重得多（没有活动标签页时也一样要拦）。
           *
           * 输入框里让位：Ctrl+W 是 readline 的「删除前一个词」，用户想删个词
           * 却把标签页连内容一起弄没了。CLI 输入框自己接管了这个键并
           * stopPropagation，压根到不了这里；其余输入框拦住不做事就够了。
           */
          event.preventDefault()
          if (typing) return
          if (!tabs.activeId) return
          tabs.close(tabs.activeId)
          return
        }
        default:
          return
      }
    }

    if (event.key === 'F5') {
      const sessionId = sessions.activeSessionId
      if (!sessionId) return
      event.preventDefault()
      void sessions.rescan(sessionId)
    }
  }

  onMounted(() => window.addEventListener('keydown', onKeydown))
  onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown))
}
