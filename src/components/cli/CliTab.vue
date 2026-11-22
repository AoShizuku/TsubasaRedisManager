<script setup lang="ts">
import { nextTick, onMounted, ref } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import { executeCli } from '@/api/redis'
import { toFault, READONLY_REJECTED } from '@/api/invoke'
import { formatElapsed } from '@/lib/format'
import { killWordBefore } from '@/lib/lineEdit'
import { asText, isServerError } from '@/lib/redisValue'
import { useSessionsStore } from '@/stores/sessions'
import { useTabsStore, type Tab } from '@/stores/tabs'
import { useUiStore } from '@/stores/ui'
import type { RedisValue } from '@/types/ipc'

/**
 * CLI 是请求/响应，不是 PTY。
 *
 * 没有 ANSI、没有行编辑器、也没有伪终端，所以用回显区 + 真实 input，
 * 而不是终端模拟器 —— 后者还得自己实现一遍 readline，白费。
 * 借了 readline 的几个键（↑↓ 历史、Ctrl+W 删词、Ctrl+L 清屏）是因为手指
 * 记着它们，不是要把行编辑器补回来。
 */
const props = defineProps<{ tab: Tab }>()

const sessions = useSessionsStore()
const tabs = useTabsStore()
const ui = useUiStore()

interface Line {
  id: number
  kind: 'cmd' | 'reply' | 'error' | 'note'
  text: string
  elapsed?: number
}

let lineId = 0
const lines = ref<Line[]>([])
const input = ref('')
const busy = ref(false)
const scroller = ref<HTMLElement | null>(null)
const inputEl = ref<HTMLInputElement | null>(null)
const history: string[] = []
let historyIndex = -1

const session = sessions.sessions.get(props.tab.sessionId)

/**
 * 焦点纪律：CLI 的「家」是命令输入框，任何离家的动作都得自带回程票 ——
 * 执行完命令、点回显区、按完清屏，焦点都要回来。
 *
 * 但**只有焦点还没离开 CLI 时才送**：用户发完一条慢命令就去点侧栏的 key 了，
 * 回包到达时把焦点抢回来，就成了系统事件抢焦点。三块表面各挂一个
 * `data-cli-surface` 来划边界，比按元素逐个判省事，也比判 body 准 ——
 * Chromium 会把焦点给隐式可聚焦的滚动容器，不一定落到 body。
 */
function focusInCli(): boolean {
  const el = document.activeElement
  if (!el || el === document.body) return true
  return el.closest('[data-cli-surface]') !== null
}

/** 不问选区直接回家。留给清屏这类自己就把选区宿主销毁掉了的动作 */
function focusInput() {
  if (!focusInCli()) return
  inputEl.value?.focus()
}

/**
 * 常规回家：**有选区就让位**。回家要 focus 输入框，而 Chromium 的 selection
 * 归属于 focused element —— 焦点一落选区当场归零，回包里的值就复制不走了。
 *
 * 判在这里而不是判在点击上：拖选的收尾固然是一次 click，但慢命令的回包
 * 同样可能正好落在拖选途中，那条路径也得让位。
 */
function returnHome() {
  const selection = window.getSelection()
  if (selection && !selection.isCollapsed) return
  focusInput()
}

// 标签页切走时 MainArea 会按 tab 重建组件，所以挂载即聚焦就够了：
// 打开 CLI 的下一秒就能敲，不用先拿鼠标点一下输入框
onMounted(() => inputEl.value?.focus())

function push(line: Omit<Line, 'id'>) {
  lines.value.push({ ...line, id: (lineId += 1) })
  void nextTick(() => {
    if (scroller.value) scroller.value.scrollTop = scroller.value.scrollHeight
  })
}

/** 按 redis-cli 的习惯渲染回包：整数带 (integer)、数组带序号、空值是 (nil)。 */
function renderReply(value: RedisValue): string {
  if (value === null) return '(nil)'
  if (typeof value === 'number') return `(integer) ${value}`
  if (typeof value === 'string') return `"${value}"`
  if (isServerError(value)) return `(error) ${value.value}`
  if (Array.isArray(value)) {
    if (!value.length) return '(empty array)'
    return value
      .map((item, index) => {
        const prefix = `${index + 1}) `
        // 嵌套数组的续行要跟首行的序号对齐，否则层级看不出来
        const [first, ...rest] = renderReply(item).split('\n')
        const pad = ' '.repeat(prefix.length)
        return [prefix + first, ...rest.map((line) => pad + line)].join('\n')
      })
      .join('\n')
  }
  return asText(value)
}

async function submit() {
  const command = input.value.trim()
  if (!command) return
  // 执行期间输入框**不锁**（命令在飞的时候照样能打字，这是终端的手感），
  // 所以重入要在这里挡住。静默吞掉回车会让人以为界面卡死了，给一句提示。
  if (busy.value) {
    ui.notify('warn', '上一条命令还在执行')
    return
  }

  history.unshift(command)
  historyIndex = -1
  input.value = ''
  push({ kind: 'cmd', text: command })

  busy.value = true
  try {
    const result = await executeCli(props.tab.sessionId, command)
    push({ kind: 'reply', text: renderReply(result.value), elapsed: result.elapsedMs })
  } catch (error) {
    const fault = toFault(error)
    push({
      kind: 'error',
      text:
        fault.code === READONLY_REJECTED
          ? `(error) 只读模式：${fault.detail ?? ''} 是写命令，已被拦截`
          : `(error) ${fault.text}`,
    })
  } finally {
    busy.value = false
    returnHome()
  }
}

function historyPrev() {
  if (!history.length) return
  historyIndex = Math.min(history.length - 1, historyIndex + 1)
  input.value = history[historyIndex]
}

function historyNext() {
  if (historyIndex <= 0) {
    historyIndex = -1
    input.value = ''
    return
  }
  historyIndex -= 1
  input.value = history[historyIndex]
}

function clear() {
  lines.value = []
  // 清屏是纯鼠标动作，焦点不该停在按钮上吃键 —— 原生 button 持焦时
  // 空格和回车都等于再点一次，用户下一个空格就又清了一遍屏。
  // 走 focusInput 而不是 returnHome：选中回显区的文字再点清屏时，
  // 选区这一刻还在（DOM 还没 patch），让位给它就等于把焦点丢在按钮上
  focusInput()
}

/**
 * Ctrl+W 是 readline 的「删除前一个词」，属于 shell 命根子键。
 * 全局快捷键层把它占成「关闭标签页」的话，用户想删个词就把命令连回显一起弄丢了。
 * 这里连 stopPropagation 一起接管，全局那层就收不到（见 useShortcuts）。
 */
function deleteWordBack() {
  const el = inputEl.value
  if (!el) return
  const from = el.selectionStart ?? input.value.length
  const { text, caret } = killWordBefore(input.value, from, el.selectionEnd ?? from)
  input.value = text
  // v-model 的新值要等 Vue patch 进 DOM，光标才放得住
  void nextTick(() => el.setSelectionRange(caret, caret))
}

</script>

<template>
  <!-- data-cli-surface 划出「家」的边界，focusInCli 靠它判要不要把焦点送回来 -->
  <div class="toolbar" data-cli-surface>
    <span class="muted mono" style="font-size: var(--fs-11)">
      {{ tabs.scopeLabel(tab.sessionId) }}
    </span>
    <span v-if="session?.readonly" class="tag tag-danger">只读</span>
    <span class="spacer" />
    <Btn size="sm" icon="rotate-ccw" @click="clear">清屏</Btn>
  </div>

  <div ref="scroller" class="term" data-cli-surface @click="returnHome">
    <div v-if="!lines.length" class="muted">
      输入 Redis 命令后回车执行。↑ / ↓ 翻历史。
    </div>
    <div v-for="line in lines" :key="line.id">
      <span v-if="line.kind === 'cmd'" class="cmd">{{ line.text }}</span>
      <span v-else-if="line.kind === 'error'" class="ret-e">{{ line.text }}</span>
      <span v-else class="ret">{{ line.text }}</span>
      <span v-if="line.elapsed !== undefined" class="ms">  · {{ formatElapsed(line.elapsed) }}</span>
    </div>
  </div>

  <div class="term-input" data-cli-surface>
    <!-- 忙态只换提示符，不锁输入框：disabled 会把焦点顶给 body，
         再放开也不会自己回来 —— 那正是每敲一条命令就得用鼠标点回来的原因 -->
    <span class="prompt">
      <Icon v-if="busy" name="loader" :size="12" />
      <template v-else>&gt;</template>
    </span>
    <input
      ref="inputEl"
      v-model="input"
      placeholder="GET user:1001"
      spellcheck="false"
      autocomplete="off"
      @keydown.enter="submit"
      @keydown.up.prevent="historyPrev"
      @keydown.down.prevent="historyNext"
      @keydown.ctrl.l.prevent="clear"
      @keydown.ctrl.w.prevent.stop="deleteWordBack"
    />
  </div>
</template>
