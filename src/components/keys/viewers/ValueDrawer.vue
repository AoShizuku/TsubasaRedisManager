<script setup lang="ts">
import { computed, onBeforeUnmount, ref, shallowRef, watch } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import CodecPicker from './CodecPicker.vue'
import { getFullValue } from '@/api/redis'
import {
  chainBytes,
  chainStruct,
  newChain,
  STRUCT_LOSSY_PREFIX,
  type CodecChain,
} from '@/lib/codec/chain'
import { renderNode } from '@/lib/codec/render'
import { formatBytes } from '@/lib/format'
import { detectShape, prettyJson } from '@/lib/redisValue'
import { bytesOf, EMPTY_TEXT, isBinaryText, isTruncated, textOf } from '@/lib/redisText'
import { useUiStore } from '@/stores/ui'
import type { RedisText, ValueRef } from '@/types/ipc'

/**
 * 值详情抽屉。表格行只放得下一行预览，完整值在这里看。
 * 头部可以上下拖动改高度 —— 看 JSON 时想大一点，扫字段时想小一点。
 *
 * 收的是 `RedisText` 而不是 `string`。调用方先 `textOf()` 转成文本再传进来的话，
 * 二进制成员会变成十六进制串，送去解码的成了「hex 文本的 base64」——
 * 任何 gzip / msgpack 的 hash 字段值在这里都必然解不开，而 hash 字段恰恰是它们
 * 最常见的存放位置；截断标记也会一并丢掉，判断不了能不能解。
 */
const props = withDefaults(
  defineProps<{
    title: string
    value: RedisText | null
    /** 重取完整值要用的会话 id。 */
    sessionId?: string | null
    /**
     * 重取完整值的定位符。为空 = 这个值没有独立的定位符（集合成员就是这样），
     * 那时不能把按钮一禁了事 —— 要用 `noTargetReason` 说清为什么。
     * 见 doc/backend/ipc-contract.md「完整值重取」。
     */
    target?: ValueRef | null
    noTargetReason?: string | null
    /**
     * 这里显示的是不是**单个**值。Stream 传进来的是整条 entry 拼成的文本，
     * 没有单个值可解，它的解码按钮是个死按钮 —— 那里要关掉而不是留着。
     */
    decodable?: boolean
  }>(),
  { decodable: true },
)

const ui = useUiStore()

const height = ref(212)
const collapsed = ref(false)
/**
 * 解码链。链尾可能是字节（压缩类）也可能是结构（序列化类），重取回来的完整值
 * 则整条链重开。
 *
 * `shallowRef`：重取回来的值可能有 8 MB；而且链里装着 `Node`，那是递归类型，
 * 用 `ref` 会让 Vue 的 `UnwrapRef` 把 TS 类型实例化撑爆（TS2589）。
 *
 * 解出来的结构**不能先 JSON 化成 `RedisText`** —— int64 精度、二进制字段、
 * 非字符串 map key 会当场丢光，正是把这些 codec 归给前端的理由被自己推翻。
 * 见 doc/frontend/codecs.md「产物是结构不是文本」。
 */
const chain = shallowRef<CodecChain>(newChain(props.value ?? EMPTY_TEXT))
const loading = ref(false)

/* 换了一行就得扔掉上一行的链，否则会把 A 的解码结果当成 B 的值显示 */
watch(
  () => props.value,
  (value) => {
    chain.value = newChain(value ?? EMPTY_TEXT)
  },
)

/** 链尾解出来的结构，没有就是 `null`。 */
const struct = computed(() => chainStruct(chain.value))

/* 抽屉没有保存路径，链在这里只影响展示 —— 看到的是哪一层，就照哪一层显示。 */
const shown = computed<RedisText | null>(() => (props.value ? chainBytes(chain.value) : null))
const text = computed(() => (shown.value ? textOf(shown.value) : ''))
const pretty = computed(() =>
  struct.value || detectShape(text.value) !== 'json' ? null : prettyJson(text.value),
)
const display = computed(() =>
  struct.value ? renderNode(struct.value.root) : (pretty.value ?? text.value),
)

/**
 * 往返自检没过时的理由。抽屉本来就没有保存路径，所以它在这里纯属如实告知：
 * 编不回原字节意味着这份呈现是有损的，照着它去判断值的内容会判错。
 *
 * 自检**通过**时这里什么都不说：那种情况下呈现是无损的，"还不能编辑"是
 * `StringViewer` 那条保存路径的事，跟这里无关。
 *
 * **`noEncoder` 也什么都不说。** 那是只读类 codec（pickle）的处境，跟 `lossy`
 * 正好相反：它的呈现是**完整准确**的，只是我们不给它写编码器
 * （doc/frontend/codecs.md「Editable 的三种说法」）。
 * 而这个横幅说的是"下面这份呈现是有损的" —— 对 pickle 那是假话，
 * 而"不能保存"在一个本来就没有保存按钮的抽屉里根本不值一提。
 */
const structNote = computed(() => {
  const editable = struct.value?.editable
  if (!editable || editable.ok || editable.kind !== 'lossy') return null
  return editable.reason
})

/** 真实字节数：二进制值显示成十六进制，按显示出来的文本数会虚报三倍。 */
const bytes = computed(() => (shown.value ? bytesOf(shown.value).length : 0))
const binary = computed(() => (shown.value ? isBinaryText(shown.value) : false))
const truncated = computed(() => (shown.value ? isTruncated(shown.value) : false))
const totalBytes = computed(() => shown.value?.totalBytes ?? 0)

/** 有定位符才重取得了。集合成员没有 —— 那时不出按钮，改成写出原因。 */
const canReload = computed(() => Boolean(props.target && props.sessionId))

async function loadFull() {
  if (!props.target || !props.sessionId) return
  loading.value = true
  try {
    const full = await getFullValue(props.sessionId, props.target)
    // 换了源字节，上一条链就不再对应屏幕上的值了
    chain.value = newChain(full)
    // 仍然截断说明值超过了后端 8 MB 的重取上限，截断提示会自己留在原地
    ui.notify('ok', isTruncated(full) ? '值超过 8 MB，取回的仍然是截断值' : '已加载完整值')
  } catch (error) {
    ui.notifyError(error, '加载完整值失败')
  } finally {
    loading.value = false
  }
}

let startY = 0
let startHeight = 0

function onMove(event: MouseEvent) {
  // 往上拖是增高，所以是 startY - clientY
  height.value = Math.min(560, Math.max(32, startHeight + startY - event.clientY))
}

function onUp() {
  window.removeEventListener('mousemove', onMove)
  window.removeEventListener('mouseup', onUp)
}

function startResize(event: MouseEvent) {
  if (event.button !== 0 || collapsed.value) return
  event.preventDefault()
  startY = event.clientY
  startHeight = height.value
  window.addEventListener('mousemove', onMove)
  window.addEventListener('mouseup', onUp)
}

onBeforeUnmount(onUp)

async function copy() {
  try {
    await navigator.clipboard.writeText(text.value)
    ui.notify('ok', '已复制')
  } catch (error) {
    ui.notifyError(error, '复制失败')
  }
}
</script>

<template>
  <div
    class="drawer"
    :class="{ 'is-collapsed': collapsed }"
    :style="{ height: `${collapsed ? 32 : height}px` }"
  >
    <div class="drawer-hd" @mousedown="startResize">
      <span class="t">{{ title }}</span>
      <span v-if="shown" class="muted num" style="font-size: var(--fs-11)">
        {{ formatBytes(bytes) }}
      </span>
      <!-- 结构先判：屏幕上是解出来的树，源字节是不是 UTF-8 已经不影响读它 -->
      <span v-if="struct" class="tag tag-accent">结构</span>
      <!-- 二进制再判：非 UTF-8 的字节渲染出来是十六进制，再谈 JSON 没有意义 -->
      <span v-else-if="binary" class="tag tag-warn">BIN</span>
      <span v-else-if="pretty" class="tag tag-accent">JSON</span>
      <!-- 收起时下面那条横幅看不见，截断这件事不能只写在那里 -->
      <span v-if="truncated" class="tag tag-warn">截断</span>
      <span class="spacer" />
      <template v-if="shown && !collapsed">
        <CodecPicker v-if="decodable" :source="shown" :chain="chain" @change="chain = $event" />
        <Btn variant="ghost" size="sm" icon="copy" icon-only title="复制" @click="copy" />
      </template>
      <Btn
        variant="ghost"
        size="sm"
        :icon="collapsed ? 'chevron-up' : 'chevron-down'"
        icon-only
        :title="collapsed ? '展开' : '收起'"
        @click="collapsed = !collapsed"
      />
    </div>

    <div class="drawer-body">
      <div v-if="!shown" class="emptyline">
        <Icon name="eye" :size="16" />
        <span>选中一行查看完整值</span>
      </div>
      <template v-else>
        <!-- 截断必须显式说出来，并且要给得出下一步；给不出就要说清为什么 -->
        <div v-if="truncated" class="banner banner-warn" style="margin: 10px 12px 0">
          <Icon name="alert-triangle" :size="16" />
          <span>
            这个值太大，只取回了前 {{ formatBytes(bytes) }}（共 {{ formatBytes(totalBytes) }}）。
            <template v-if="canReload">压缩与序列化格式都要完整字节才解得开。</template>
            <template v-else>
              {{ noTargetReason ?? '这个值没有可以用来重取的定位符。' }}
            </template>
          </span>
          <span class="spacer" />
          <Btn v-if="canReload" size="sm" icon="download" :disabled="loading" @click="loadFull">
            {{ loading ? '加载中…' : '加载完整值' }}
          </Btn>
        </div>
        <!-- 往返自检没过：解出来的结构编不回原字节，说明这份呈现是有损的。
             这里本来就没有保存路径，所以只需要如实说，不用改任何按钮状态。 -->
        <div v-if="structNote" class="banner banner-warn" style="margin: 10px 12px 0">
          <Icon name="alert-triangle" :size="16" />
          <span>{{ STRUCT_LOSSY_PREFIX }}（{{ structNote }}），下面这份呈现是有损的。</span>
        </div>
        <pre class="term selectable" style="padding: 10px 12px">{{ display }}</pre>
      </template>
    </div>
  </div>
</template>
