<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, shallowRef, watch } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import Seg from '@/components/ui/Seg.vue'
import CodecPicker from './CodecPicker.vue'
import { getStringValue, setStringValue } from '@/api/redis'
import {
  appliedCodecs,
  chainBytes,
  chainLabel,
  chainStruct,
  encodeAlongChain,
  newChain,
  rebaseChain,
  saveBlockedReason,
  type CodecChain,
} from '@/lib/codec/chain'
import { renderNode } from '@/lib/codec/render'
import type { EditorHandle } from '@/lib/monaco'
import { loadMonaco } from '@/lib/monacoLoader'
import { formatBytes, formatCount } from '@/lib/format'
import { detectShape, prettyJson } from '@/lib/redisValue'
import {
  base64FromBytes,
  bytesOf,
  EMPTY_TEXT,
  isBinaryText,
  isTruncated,
  textOf,
  textValue,
} from '@/lib/redisText'
import { capabilityBlockedReason } from '@/lib/capabilities'
import { useSessionsStore } from '@/stores/sessions'
import { useSettingsStore } from '@/stores/settings'
import { useUiStore } from '@/stores/ui'
import type { RedisText } from '@/types/ipc'

const props = defineProps<{ sessionId: string; keyName: string; readonly: boolean }>()
const emit = defineEmits<{ changed: [] }>()

const sessions = useSessionsStore()
const settings = useSettingsStore()
const ui = useUiStore()

const host = ref<HTMLElement | null>(null)
const editor = shallowRef<EditorHandle | null>(null)

/*
 * 读回来的值就存在链的源头上，不另存一份 —— 两份会在保存之后走散。
 *
 * shallowRef：值可能很大，没必要给它做响应式代理；而且链里装着 `Node`，
 * 那是递归类型，`ref` 会让 Vue 的 `UnwrapRef` 把 TS 类型实例化撑爆（TS2589）。
 */
const chain = shallowRef<CodecChain>(newChain(EMPTY_TEXT))
const text = ref('')
const binary = ref(false)
/** 后端截断过的值一律禁止保存 —— 写回去会静默丢掉后面的部分 */
const truncated = ref(false)
/**
 * 编辑器里现在显示的是**解出来的结构**而不是值本身，非空即"为什么不能保存"。
 *
 * 与 `truncated` 是同构的（都是"这个值不能安全写回"），所以走同一条只读路径。
 * 两种失败是不同性质的事（数据编不回原字节 / 还没有结构编辑器），措辞由
 * `saveBlockedReason` 分开给，见 doc/frontend/codecs.md「Editable 的三种说法」。
 */
const structReason = ref<string | null>(null)
const dirty = ref(false)
const loading = ref(true)
/** 编辑器建好了没。它和 `loading` 现在是两件事，见 `mountEditor`。 */
const editorReady = ref(false)
const saving = ref(false)
const language = ref<'json' | 'plaintext'>('plaintext')

/** 链上当前这一层的字节。空链时就是 `raw`。 */
const current = computed(() => chainBytes(chain.value))

/* 结构与二进制视图里编辑器装的都是**渲染产物**（带标注的结构文本 / 十六进制），
   量它的长度没有意义 —— 报这一层的源字节数。 */
const byteLength = computed(() =>
  structReason.value || binary.value
    ? bytesOf(current.value).length
    : new TextEncoder().encode(text.value).length,
)

/* 回退视图不虚拟化，超过这行数就只画前面这些。它只活到编辑器就绪为止，
   在这里铺十万行 DOM 只会把它本来要省下的那点时间又赔进去。 */
const FALLBACK_LINES = 400

/** 编辑器就绪前的静态预览。只在真的要显示时才算 —— computed 是惰性的。 */
const fallback = computed(() => {
  const lines = text.value.split('\n')
  const shown = lines.slice(0, FALLBACK_LINES)
  return {
    gutter: shown.map((_, index) => index + 1).join('\n'),
    code: shown.join('\n'),
    rest: lines.length - shown.length,
  }
})

const modeOptions = [
  { value: 'plaintext', label: '纯文本' },
  { value: 'json', label: 'JSON' },
]

/* 动态 import 之间组件可能已经被卸载（快速切标签页），那时再 create 就会把一个
   编辑器实例连同它的 worker 一起漏掉。取值那条路现在也要看它 —— 并行之后
   GET 可能在卸载之后才回来。 */
let disposed = false

/**
 * 把当前 text / language / 只读态推给编辑器，并把它当成新的干净起点。
 *
 * 三处（读取完成、换编解码层、编辑器刚建好）都收敛到这一个函数。各设各的迟早
 * 会漂开，`readOnly` 就已经漏过一次：`createEditor` 传的是 `props.readonly`，
 * 少了截断 / 二进制 / 结构这三种只读，从前靠 create 必定先于 load 才没出事。
 *
 * 值先回来时 `editor` 还是 null，这里整个空转，由后到的 `mountEditor` 再调一次。
 * `setValue` 会触发 `onChange`，所以 dirty 必须在它之后清。
 */
function syncEditor() {
  editor.value?.setValue(text.value)
  editor.value?.setLanguage(language.value)
  editor.value?.updateOptions({ readOnly: readOnlyNow() })
  dirty.value = false
}

async function load() {
  loading.value = true
  try {
    const value = await getStringValue(props.sessionId, props.keyName)
    if (disposed) return
    chain.value = newChain(value)
    binary.value = isBinaryText(value)
    truncated.value = isTruncated(value)
    structReason.value = null
    text.value = textOf(value)

    // JSON 检测只在打开时做一次：值可能很大，不该每次按键都 parse
    const shape = detectShape(text.value)
    if (shape === 'json') {
      language.value = 'json'
      if (settings.state.formatJsonOnOpen) {
        text.value = prettyJson(text.value) ?? text.value
      }
    } else {
      language.value = 'plaintext'
    }

    syncEditor()
  } catch (error) {
    // 标签页都关掉了还弹一条「读取失败」只会让人莫名其妙
    if (!disposed) ui.notifyError(error, '读取失败')
  } finally {
    loading.value = false
  }
}

/**
 * 加载 Monaco 并建编辑器。
 *
 * Monaco 接近 4 MB，动态 import 让它只在真正打开 STRING 编辑器时才下载解析。
 * 静态 import 会把它挂到主 chunk 的依赖图上，等于没拆。
 */
async function mountEditor() {
  if (!host.value) return
  const { createEditor } = await loadMonaco()
  if (disposed || !host.value) return
  const handle = createEditor(host.value, {
    value: text.value,
    language: language.value,
    // 不是 props.readonly：值可能已经先回来了，那时只读性由 readOnlyNow 说了算，
    // 传 props.readonly 会把截断 / 二进制 / 结构的只读保护抹掉
    readOnly: readOnlyNow(),
    wordWrap: settings.state.editorWordWrap,
    tabSize: settings.state.editorTabSize,
    minimap: settings.state.editorMinimap,
  })
  handle.onChange(() => {
    text.value = handle.getValue()
    dirty.value = true
  })
  editor.value = handle
  syncEditor()
  /* 等一帧再撤掉回退视图。`setValue` 只是把渲染排进了 Monaco 自己的下一帧，
     此刻就撤会露出一帧空编辑器 —— 底色一样所以不闪白，但文字会眨一下。
     Monaco 的回调排在这个 rAF 之前，所以两件事落在同一帧里。 */
  requestAnimationFrame(() => {
    editorReady.value = true
  })
}

/*
 * 读值与装配编辑器**并行**跑。
 *
 * 从前是 `await import(monaco)` → `createEditor` → 才发 GET，于是首次打开一个
 * String 的等待里有一大半是编辑器冷启动，而界面上一直写着「正在读取…」——
 * 把编辑器初始化伪装成了读 Redis。两条路谁先完成都要能收场，汇合点在
 * `syncEditor`；值先到就先用 `.ed-fallback` 把内容摆出来。
 */
onMounted(() => {
  void load()
  void mountEditor()
})

onBeforeUnmount(() => {
  disposed = true
  editor.value?.dispose()
})

watch(
  () => [settings.state.editorWordWrap, settings.state.editorMinimap] as const,
  ([wrap, minimap]) =>
    editor.value?.updateOptions({
      wordWrap: wrap ? 'on' : 'off',
      minimap: { enabled: minimap },
    }),
)

watch(language, (value) => editor.value?.setLanguage(value))

/**
 * 保存要**沿解码链反向 encode**。
 *
 * 解码之后 `text.value` 装的是解压出来的明文，直接 `base64FromText(text.value)`
 * 写回去就会**覆盖掉原本的压缩值**；"RoundTrip" 这个词的意思正在于此。
 * 见 doc/frontend/codecs.md「链」。
 *
 * 空链时 `encodeAlongChain` 原样返回，退化成从前那一行。
 */
async function save() {
  saving.value = true
  try {
    const codecs = appliedCodecs(chain.value)
    const levels = await encodeAlongChain(codecs, new TextEncoder().encode(text.value))
    await setStringValue(props.sessionId, props.keyName, base64FromBytes(levels[0]))
    // 写完把链上的字节换成刚写进去的那份，否则"还原原始值"会掏出一份
    // 已经不在 Redis 里的旧值 —— 界面会说谎，而且说的是最难被怀疑的那种谎
    chain.value = rebaseChain(chain.value, levels)
    dirty.value = false
    ui.notify('ok', codecs.length ? `已按 ${chainLabel(chain.value)} 重新编码并保存` : '已保存')
    emit('changed')
  } catch (error) {
    ui.notifyError(error, '保存失败')
  } finally {
    saving.value = false
  }
}

/**
 * 只读的来源合在一处，免得编辑器与保存按钮各判各的。
 *
 * `binary` 也在里面：编辑器里那串十六进制是**渲染产物**，照着存会把十六进制
 * 文本本身写进 Redis。这与截断、结构是同一件事（"屏幕上的不是值"），
 * `HashViewer` 的编辑对话框早就这么挡了，只有这里一直漏着。
 */
function readOnlyNow(): boolean {
  return props.readonly || truncated.value || binary.value || structReason.value !== null
}

/** 链变了（解了一层、撤销一步、还原）：编辑器跟着换成这一层的样子。 */
function onChain(next: CodecChain) {
  chain.value = next
  const struct = chainStruct(next)
  if (struct) {
    // 结构不能 JSON 化回文本再显示（会丢 int64 精度与二进制字段），
    // 所以走 render.ts 的带类型标注的展示文本。见 doc/frontend/codecs.md「产物是结构不是文本」。
    binary.value = false
    truncated.value = false
    structReason.value = saveBlockedReason(next)
    text.value = renderNode(struct.root)
    language.value = 'plaintext'
  } else {
    // 解码产物未必是文本：gzip 里可能裹着 msgpack，那时只能按十六进制看
    const value = chainBytes(next)
    binary.value = isBinaryText(value)
    truncated.value = isTruncated(value)
    structReason.value = null
    text.value = textOf(value)
    // 解码结果通常是 JSON，顺手切一下语言
    if (detectShape(text.value) === 'json') language.value = 'json'
  }
  syncEditor()
}

/* 解码的输入是链上**这一层的字节**，不是编辑器里显示的文本 ——
   二进制值在编辑器里已经被渲染成十六进制了，拿它去解码只会失败。
   改过之后解的是屏幕上那份：链只记录用过哪些 codec，反向 encode 是对
   这张表求逆，中途改过字节不影响它成立。 */
const codecSource = computed<RedisText>(() =>
  dirty.value && !structReason.value ? textValue(text.value) : current.value,
)

/**
 * 保存要保留原有过期时间：KEEPTTL 不可用时后端改走脚本，脚本也不可用就只能拒绝 ——
 * 绝不退回裸 SET 悄悄清掉 TTL。见 doc/backend/capabilities.md「STRING 保存」。
 * 原因用后端的 reason，不自己拼服务端版本。
 */
const keepTtlBlocked = computed(() => {
  const capabilities = sessions.sessions.get(props.sessionId)?.capabilities
  if (capabilities?.setKeepTtl.state !== 'unsupported') return null
  const scripting = capabilityBlockedReason(capabilities, 'scripting')
  return scripting ? `${capabilities.setKeepTtl.reason}；${scripting}` : null
})

const saveTitle = computed(() => {
  if (props.readonly) return '该连接为只读模式'
  if (keepTtlBlocked.value) return keepTtlBlocked.value
  if (truncated.value) return '值已被截断，保存会丢掉未显示的部分'
  if (binary.value) return '这一层不是 UTF-8，编辑器里显示的是十六进制'
  if (structReason.value) return structReason.value
  // 解过码就得说清楚写回去的是什么：屏幕上是明文，落进 Redis 的是压缩字节
  if (chain.value.steps.length) return `保存会按 ${chainLabel(chain.value)} 重新编码后写回`
  return undefined
})
</script>

<template>
  <div class="toolbar">
    <Seg v-model="language" :options="modeOptions" />
    <!-- 编辑器就绪前这个按钮点下去是空转（`editor` 还是 null），而屏幕上摆着的
         是静态回退视图 —— 该禁用而不是假装能用 -->
    <Btn
      size="sm"
      icon="wand"
      :disabled="language !== 'json' || !editorReady"
      title="格式化 JSON"
      @click="editor?.format()"
    >
      格式化
    </Btn>
    <span class="vsep" />
    <CodecPicker :source="codecSource" :chain="chain" @change="onChain" />

    <span class="spacer" />
    <span v-if="structReason" class="tag tag-accent">结构</span>
    <span v-else-if="binary" class="tag tag-warn">二进制</span>
    <span class="muted num" style="font-size: var(--fs-11)">{{ formatBytes(byteLength) }}</span>
    <span class="vsep" />
    <Btn
      variant="primary"
      size="sm"
      icon="check"
      :disabled="readonly || keepTtlBlocked !== null || truncated || binary || Boolean(structReason) || !dirty || saving"
      :title="saveTitle"
      @click="save"
    >
      保存
    </Btn>
  </div>

  <!-- 结构视图排在最前：此刻编辑器里根本不是这个 key 的字节，
       再提"值太大"或"不是 UTF-8"只会误导 -->
  <div v-if="structReason" class="banner banner-warn" style="margin: 10px 14px 0">
    <Icon name="alert-triangle" :size="16" />
    <span>{{ structReason }}编辑器已切换为只读。</span>
  </div>

  <div v-else-if="truncated" class="banner banner-warn" style="margin: 10px 14px 0">
    <Icon name="alert-triangle" :size="16" />
    <span>
      这个值太大，只显示了前 {{ formatBytes(byteLength) }}（共
      {{ formatBytes(current.totalBytes ?? 0) }}）。为避免截断后保存把剩下的部分丢掉，
      编辑器已切换为只读。
    </span>
  </div>

  <div v-else-if="binary" class="banner banner-warn" style="margin: 10px 14px 0">
    <Icon name="alert-triangle" :size="16" />
    <span>
      {{ chain.steps.length ? '解出来的这一层' : '这个值' }}不是合法 UTF-8，上面显示的是十六进制。
      照着这串十六进制保存等于把它当成值写进去，所以编辑器已切换为只读 ——
      请再套一层编解码器，或改用命令行写入。
    </span>
  </div>

  <div class="editor">
    <div ref="host" class="monaco-host" />
    <!-- 编辑器就绪前盖在它上面。从前这里只有一个「正在读取…」，而它盖住的大半
         是 Monaco 的冷启动 —— 分成两态之后，转圈的时候是真的在读 Redis，
         值一到就先把内容摆出来，编辑器随后接手。 -->
    <div v-if="!editorReady" class="ed-overlay">
      <div v-if="loading" class="emptyline">
        <Icon name="loader" :size="16" />
        <span>正在读取…</span>
      </div>
      <template v-else>
        <div class="ed-fallback">
          <div class="ed-gutter num">{{ fallback.gutter }}</div>
          <div class="ed-code">{{ fallback.code }}</div>
        </div>
        <div v-if="fallback.rest > 0" class="ed-more num">
          还有 {{ formatCount(fallback.rest) }} 行，编辑器就绪后显示
        </div>
      </template>
    </div>
  </div>
</template>
