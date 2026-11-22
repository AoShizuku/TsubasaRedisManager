<script setup lang="ts">
import { computed, nextTick, onMounted, ref, shallowRef, watch } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import Seg from '@/components/ui/Seg.vue'
import { addHllElements, getHllCount, getStringValue, inspectHll } from '@/api/redis'
import { toFault, type AppFault } from '@/api/invoke'
import { formatBytes, formatCount } from '@/lib/format'
import { base64FromText, bytesFromBase64, bytesOf } from '@/lib/redisText'
import { useUiStore } from '@/stores/ui'
import type { HllInspect, RedisText } from '@/types/ipc'

/**
 * HyperLogLog 查看器。HLL 是 string 的子形态（TYPE 就返回 string），
 * KeyTab 靠 `meta.hll` 分发到这里。
 *
 * 它没有成员表 —— HLL 只存概率寄存器，成员不可枚举、不可检索、不可删除，
 * 所以主体是估算基数的仪表盘；唯一的写操作是 PFADD。
 * 结构查看器（头部解析 + 寄存器热力图 / 原始 hex）健康态收在底部抽屉里，
 * PFCOUNT 被服务端拒绝（WRONGTYPE / INVALIDOBJ，伪 HLL 或已损坏）时顶到主体、只读展示。
 *
 * 刻意不提供原始字节编辑：裸编辑保存即损坏，这正是本查看器要堵住的路径。
 * 真要改字节，CLI（GET / SET）是正当入口。见 doc/frontend/hll-viewer.md「不提供原始字节编辑」。
 */
const props = defineProps<{ sessionId: string; keyName: string; readonly: boolean }>()
const emit = defineEmits<{ changed: [] }>()

const ui = useUiStore()

/* ── 估算基数 ─────────────────────────────────────────── */

const count = ref<number | null>(null)
const countFault = ref<AppFault | null>(null)
const countLoading = ref(true)

/**
 * 回退态的判据是**服务端说这不是可用的 HLL**，两种措辞都要认：
 * 头部 / 长度没过校验是 `WRONGTYPE Key is not a valid HyperLogLog string value`，
 * sparse 操作码在计数中途坏掉是 `INVALIDOBJ Corrupted HLL object detected`。
 * 网络断掉、会话失效也会让 PFCOUNT 失败，那不是损坏 ——
 * 走普通错误行 + 重试，别拿红色 banner 误导用户。
 */
const corrupted = computed(
  () => countFault.value !== null && /WRONGTYPE|INVALIDOBJ/i.test(countFault.value.text),
)
const otherFault = computed(() => (countFault.value && !corrupted.value ? countFault.value : null))

async function loadCount() {
  countLoading.value = true
  countFault.value = null
  try {
    count.value = await getHllCount(props.sessionId, props.keyName)
  } catch (error) {
    countFault.value = toFault(error)
  } finally {
    countLoading.value = false
  }
}

/** 标准误差 ±0.81% 的绝对量。小到没意义（<10）就只报百分比。 */
const errorAbs = computed(() => {
  if (count.value === null) return null
  const abs = Math.round(count.value * 0.0081)
  return abs >= 10 ? abs : null
})

/* ── 结构查看器（解析 / 原始）─────────────────────────── */

const collapsed = ref(true)
const view = ref<'parsed' | 'raw'>('parsed')
const viewOptions = [
  { value: 'parsed', label: '解析' },
  { value: 'raw', label: '原始' },
]

const inspect = shallowRef<HllInspect | null>(null)
const inspectLoading = ref(false)
const raw = shallowRef<RedisText | null>(null)
const rawLoading = ref(false)

async function ensureInspect() {
  if (inspect.value || inspectLoading.value) return
  inspectLoading.value = true
  try {
    inspect.value = await inspectHll(props.sessionId, props.keyName)
  } catch (error) {
    ui.notifyError(error, '结构读取失败')
  } finally {
    inspectLoading.value = false
  }
}

async function ensureRaw() {
  if (raw.value || rawLoading.value) return
  rawLoading.value = true
  try {
    raw.value = await getStringValue(props.sessionId, props.keyName)
  } catch (error) {
    ui.notifyError(error, '读取失败')
  } finally {
    rawLoading.value = false
  }
}

function ensureStructData() {
  // 解析视图的字段表也要 actualBytes，所以两个视图都先保证 inspect 在
  void ensureInspect()
  if (view.value === 'raw') void ensureRaw()
}

function toggleDrawer() {
  if (corrupted.value) return // 回退态下结构就是主体，没有折叠一说
  collapsed.value = !collapsed.value
  if (!collapsed.value) ensureStructData()
}

watch(view, () => {
  if (!collapsed.value || corrupted.value) ensureStructData()
})

/* 进入回退态：结构顶到主体、原始为主（解析不了的东西，字节是唯一诚实的展示） */
watch(corrupted, (bad) => {
  if (bad) {
    collapsed.value = false
    view.value = 'raw'
    ensureStructData()
    void ensureRaw()
  }
})

/* ── 解析视图：头部字段 ───────────────────────────────── */

function hex(byte: number): string {
  return byte.toString(16).padStart(2, '0')
}

/**
 * 头部 16 字节的染色 hex。从解析结果**无损重建**（魔数与保留字节是常量，
 * encoding 与基数缓存可逆），解析视图因此不必先把原始值搬过 IPC。
 */
const headerHex = computed(() => {
  const header = inspect.value?.header
  if (!header) return null
  const card = BigInt(header.cachedCard) | (header.cacheStale ? 1n << 63n : 0n)
  const cardBytes: string[] = []
  for (let i = 0n; i < 8n; i += 1n) cardBytes.push(hex(Number((card >> (8n * i)) & 0xffn)))
  return {
    magic: '48 59 4c 4c',
    enc: header.encoding === 'dense' ? '00' : '01',
    resv: '00 00 00',
    card: cardBytes.join(' '),
  }
})

/** 寄存器区长度校验。dense 定长 12,288 B；sparse 变长，能展开就算通过。 */
const lengthCheck = computed(() => {
  const info = inspect.value
  if (!info?.header) return null
  const actual = Math.max(0, info.actualBytes - 16)
  if (info.header.encoding === 'dense') {
    return { actual, expected: 12288, ok: actual === 12288 }
  }
  return { actual, expected: null, ok: info.registersBase64 !== null }
})

/* ── 解析视图：寄存器热力图 ───────────────────────────── */

const registers = computed<Uint8Array | null>(() =>
  inspect.value?.registersBase64 ? bytesFromBase64(inspect.value.registersBase64) : null,
)

const missingRegisters = computed(() =>
  inspect.value ? inspect.value.totalRegisters - inspect.value.validRegisters : 0,
)

const regStats = computed(() => {
  const data = registers.value
  const info = inspect.value
  if (!data || !info) return null
  let nonzero = 0
  let max = 0
  let sum = 0
  for (let i = 0; i < info.validRegisters; i += 1) {
    const value = data[i]
    if (value > 0) nonzero += 1
    if (value > max) max = value
    sum += value
  }
  return { nonzero, max, mean: info.validRegisters ? sum / info.validRegisters : 0 }
})

/**
 * 非零率低于一成算低基数。「均匀噪点即健康；大块纯色是损坏」那套阅读语法
 * 只在高填充时成立 —— 低基数的画面本来就以纯黑为主，对着一张 99% 纯黑的图
 * 说"大块纯色是损坏的形状"，健康的 HLL 会被读成坏的。
 */
const LOW_FILL_RATIO = 0.1
const lowFill = computed(() => {
  const stats = regStats.value
  const valid = inspect.value?.validRegisters ?? 0
  if (!stats || valid === 0) return false
  return stats.nonzero / valid < LOW_FILL_RATIO
})

const regionCaption = computed(() => {
  const info = inspect.value
  if (!info) return ''
  const actual = Math.max(0, info.actualBytes - 16)
  // sparse 的字节是操作码流，不是 6-bit 打包 —— 「43 B · 16,384 × 6 bit」
  // 两个数字放一起是自相矛盾的
  const base =
    info.header?.encoding === 'sparse'
      ? `${formatBytes(actual)} 操作码流 · 展开为 ${formatCount(info.totalRegisters)} 寄存器`
      : `${formatBytes(actual)} · ${formatCount(info.totalRegisters)} × 6 bit`
  return missingRegisters.value > 0
    ? `${base} · 缺失 ${formatCount(missingRegisters.value)} 个寄存器`
    : base
})

const heatmap = ref<HTMLCanvasElement | null>(null)

/**
 * 一寄存器一像素（128 × 128），灰度 = 寄存器值，0 为纯黑。
 * 截断缺失的段留透明，容器的斜纹底透出来 —— 缺数据与值为零是两回事。
 */
function drawHeatmap() {
  const canvas = heatmap.value
  const data = registers.value
  const info = inspect.value
  if (!canvas || !data || !info) return
  const ctx = canvas.getContext('2d')
  if (!ctx) return
  const image = ctx.createImageData(128, 128)
  const valid = Math.min(info.validRegisters, data.length)
  for (let i = 0; i < valid; i += 1) {
    const value = Math.min(data[i], 15)
    const lum = value === 0 ? 0 : Math.round(34 + (value / 15) * 196)
    const offset = i * 4
    image.data[offset] = lum
    image.data[offset + 1] = lum
    image.data[offset + 2] = lum
    image.data[offset + 3] = 255
  }
  ctx.putImageData(image, 0, 0)
}

watch([registers, view, collapsed], async () => {
  await nextTick()
  drawHeatmap()
})

/* ── 原始视图：hex + ASCII ────────────────────────────── */

/** 平铺 hex 不虚拟化，封个顶：16 KiB = 1,024 行，覆盖 dense 全量（12,304 B）。 */
const RAW_PREVIEW_LIMIT = 16 * 1024

interface HexLine {
  offset: string
  hex: string
  ascii: string
}

function asciiOf(chunk: Uint8Array): string {
  let out = ''
  for (const byte of chunk) out += byte >= 0x20 && byte <= 0x7e ? String.fromCharCode(byte) : '.'
  return out
}

/** 整行 hex 的定宽：8 字节 × 3 − 1，组间双空格，再 8 字节 × 3 − 1。 */
const HEX_LINE_WIDTH = 48

/**
 * 组间距与行宽全部在这里拼好，模板里不留任何裸空白 ——
 * Vue 的 whitespace condense 会把模板文本里的连续空格压成一个，
 * 首行（插值写死的双空格）和正文（模板字面双空格）就差了一格。
 * 补齐到定宽是为了让末尾不满 16 字节的行的 ASCII 列也对齐。
 */
function hexOf(chunk: Uint8Array): string {
  const parts = Array.from(chunk, hex)
  const head = parts.slice(0, 8).join(' ')
  const tail = parts.slice(8).join(' ')
  return (tail ? `${head}  ${tail}` : head).padEnd(HEX_LINE_WIDTH, ' ')
}

const rawBytes = computed(() => (raw.value ? bytesOf(raw.value) : null))

const hexLines = computed<HexLine[]>(() => {
  const bytes = rawBytes.value
  if (!bytes) return []
  const shown = bytes.subarray(0, RAW_PREVIEW_LIMIT)
  const lines: HexLine[] = []
  for (let start = 0; start < shown.length; start += 16) {
    const chunk = shown.subarray(start, start + 16)
    lines.push({
      offset: start.toString(16).padStart(4, '0'),
      hex: hexOf(chunk),
      ascii: asciiOf(chunk),
    })
  }
  return lines
})

/** 首行是合法头部时按字段染色显示，与解析视图的图例同色对照。 */
const rawHeaderLine = computed(() => {
  const bytes = rawBytes.value
  if (!bytes || bytes.length < 16 || !inspect.value?.header) return null
  const segment = (from: number, to: number) => Array.from(bytes.subarray(from, to), hex).join(' ')
  return {
    magic: segment(0, 4),
    enc: segment(4, 5),
    resv: segment(5, 8),
    card: segment(8, 16),
    ascii: asciiOf(bytes.subarray(0, 16)),
  }
})

const hexBody = computed(() => (rawHeaderLine.value ? hexLines.value.slice(1) : hexLines.value))
const rawTotal = computed(() => raw.value?.totalBytes ?? rawBytes.value?.length ?? 0)
const rawShown = computed(() => Math.min(rawBytes.value?.length ?? 0, RAW_PREVIEW_LIMIT))

/* ── PFADD ────────────────────────────────────────────── */

const element = ref('')
const adding = ref(false)

async function add() {
  const value = element.value
  if (!value || adding.value || props.readonly || corrupted.value) return
  adding.value = true
  try {
    const changed = await addHllElements(props.sessionId, props.keyName, [base64FromText(value)])
    element.value = ''
    ui.notify('ok', changed ? '已添加，基数已变化' : '已添加，基数未变化（元素可能已计入）')
    emit('changed')
  } catch (error) {
    ui.notifyError(error, '添加失败')
  } finally {
    adding.value = false
  }
}

const addTitle = computed(() => {
  if (props.readonly) return '该连接为只读模式'
  if (corrupted.value) return '该值未通过 HyperLogLog 校验，无法添加元素'
  return undefined
})

onMounted(() => {
  void loadCount()
})
</script>

<template>
  <div class="toolbar">
    <div class="hll-add">
      <input
        v-model="element"
        class="input mono"
        :disabled="readonly || corrupted || adding"
        :placeholder="corrupted ? 'PFADD 不可用' : '添加元素，回车提交 · PFADD'"
        :title="addTitle"
        @keydown.enter="add"
      />
    </div>
    <Btn
      size="sm"
      icon="plus"
      :disabled="readonly || corrupted || adding || !element"
      :title="addTitle"
      @click="add"
    >
      添加
    </Btn>
    <span class="spacer" />
    <span v-if="corrupted" class="tag tag-danger">
      <Icon name="alert-triangle" :size="12" />校验失败
    </span>
    <span v-else class="muted" style="font-size: var(--fs-11)">
      已计入的元素重复添加不会改变计数
    </span>
  </div>

  <!-- 普通错误（网络 / 会话失效）：不是损坏，别用回退态误导 -->
  <div v-if="otherFault" class="emptyline">
    <Icon name="alert-circle" :size="16" />
    <span>{{ otherFault.text }}</span>
    <Btn size="sm" icon="refresh" style="margin-top: 10px" @click="loadCount">重试</Btn>
  </div>

  <template v-else>
    <!-- 健康态：基数仪表盘 -->
    <div v-if="!corrupted" class="hllboard">
      <div class="hb-cap">估算基数</div>
      <div class="hb-num">{{ countLoading || count === null ? '—' : formatCount(count) }}</div>
      <div class="hb-err">
        标准误差 ±0.81%<template v-if="errorAbs !== null"> · 约 ±{{ formatCount(errorAbs) }}</template>
      </div>
      <div class="hb-note">
        <Icon name="info" :size="12" />
        <div>
          HyperLogLog 以 16,384 个概率寄存器估算去重计数，<b>不存储元素本身</b> ——
          无法列出、检索或删除已计入的成员，因此这里没有成员表。
          合并多个 HLL 请在 CLI 中使用 <code>PFMERGE</code>。
        </div>
      </div>
    </div>

    <!-- 回退态：服务端拒绝按 HLL 计数 -->
    <div v-else class="banner banner-danger" style="margin: 10px 14px 0">
      <Icon name="alert-triangle" :size="16" />
      <div>
        <b>服务端拒绝按 HyperLogLog 计数</b>
        <div class="mono" style="margin-top: 3px">{{ countFault?.text }}</div>
        <div style="margin-top: 3px">
          头部带有 HYLL 魔数，但数据未通过服务端校验 —— 该值可能已被其他客户端截断或改写。
          以下按原始字节只读显示。
        </div>
      </div>
    </div>

    <!-- 结构查看器：健康态是抽屉（默认收起），回退态顶到主体 -->
    <div
      class="drawer hlldrawer"
      :class="{ 'is-collapsed': !corrupted && collapsed, 'is-main': corrupted }"
    >
      <div
        class="drawer-hd"
        :style="corrupted ? undefined : 'cursor: pointer'"
        @click="toggleDrawer"
      >
        <Icon v-if="!corrupted" :name="collapsed ? 'chevron-right' : 'chevron-down'" :size="12" />
        <span class="t">结构与原始字节</span>
        <span class="tag">只读</span>
        <span v-if="inspect" class="tag num">{{ formatBytes(inspect.actualBytes) }}</span>
        <span class="spacer" />
        <span @click.stop>
          <Seg v-model="view" :options="viewOptions" />
        </span>
      </div>

      <div class="drawer-body hllstruct">
        <!-- 解析：头部字段表 + 寄存器热力图 -->
        <div v-if="view === 'parsed'" class="hxp">
          <div v-if="inspectLoading && !inspect" class="emptyline">
            <Icon name="loader" :size="16" />
            <span>正在读取…</span>
          </div>
          <template v-else-if="inspect">
            <div class="hx-sec hx-header">
              <div class="hx-cap">头部 · 16 B</div>
              <template v-if="inspect.header && headerHex">
                <div class="hx-line">
                  <span class="hxf hxf-magic">{{ headerHex.magic }}</span>
                  <span class="hxf hxf-enc">{{ headerHex.enc }}</span>
                  <span class="hxf hxf-resv">{{ headerHex.resv }}</span>
                  <span class="hxf hxf-card">{{ headerHex.card }}</span>
                </div>
                <table class="hxt">
                  <tbody>
                    <tr>
                      <td class="f"><span class="swb sw-magic" />魔数</td>
                      <td class="b">48 59 4c 4c</td>
                      <td class="v">"HYLL" ✓</td>
                    </tr>
                    <tr>
                      <td class="f"><span class="swb sw-enc" />encoding</td>
                      <td class="b">{{ headerHex.enc }}</td>
                      <td class="v">
                        {{ inspect.header.encoding }}
                        {{ inspect.header.encoding === 'dense' ? '· 定长寄存器阵列' : '· 变长操作码流' }}
                      </td>
                    </tr>
                    <tr>
                      <td class="f"><span class="swb sw-resv" />保留</td>
                      <td class="b">00 00 00</td>
                      <td class="v">全零 ✓</td>
                    </tr>
                    <tr>
                      <td class="f"><span class="swb sw-card" />基数缓存</td>
                      <td class="b">{{ headerHex.card }}</td>
                      <td class="v">
                        u64 小端 = {{ formatCount(inspect.header.cachedCard) }}
                        <div class="hx-dim">
                          失效位（最高 bit）=
                          {{ inspect.header.cacheStale ? '1 · 缓存不可信' : '0 · 缓存可信' }}
                        </div>
                      </td>
                    </tr>
                    <tr v-if="lengthCheck">
                      <td class="f"><span class="swb" style="background: transparent" />寄存器区长度</td>
                      <td class="b">—</td>
                      <td class="v" :style="lengthCheck.ok ? undefined : 'color: var(--brand)'">
                        {{ formatBytes(lengthCheck.actual)
                        }}<template v-if="lengthCheck.expected !== null">
                          · 应为 {{ formatBytes(lengthCheck.expected) }}</template
                        >
                        {{ lengthCheck.ok ? '✓' : '✗' }}
                      </td>
                    </tr>
                  </tbody>
                </table>
              </template>
              <div v-else class="hx-dim">头部未通过校验 —— 这可能不是 HyperLogLog，请看原始字节。</div>
            </div>

            <div class="hx-sec">
              <div class="hx-cap">寄存器区 · {{ regionCaption }}</div>
              <div v-if="registers" class="hx-reg">
                <div class="hmwrap">
                  <canvas ref="heatmap" width="128" height="128" class="hm" />
                </div>
                <div class="hx-stats">
                  <template v-if="regStats && inspect.validRegisters > 0">
                    非零寄存器
                    <b class="num">
                      {{ formatCount(regStats.nonzero) }} / {{ formatCount(inspect.validRegisters) }}
                    </b>
                    <br />
                    <!-- 低基数时平均值四舍五入成 0.0，没有信息量，不显示 -->
                    最大 <b class="num">{{ regStats.max }}</b>
                    <template v-if="!lowFill">
                      · 平均 <b class="num">{{ regStats.mean.toFixed(1) }}</b>
                    </template>
                    <br />
                    <span class="hx-dim">亮度 = 寄存器值，0 为纯黑</span><br />
                    <span v-if="lowFill" class="hx-dim">
                      基数远小于寄存器数，以纯黑为主是正常的；亮点 = 被命中的寄存器，应均匀散布
                    </span>
                    <span v-else class="hx-dim">均匀噪点即健康；大块纯色或条纹是损坏的形状</span>
                  </template>
                  <div v-if="missingRegisters > 0" class="hx-dim" style="margin-top: 6px">
                    斜纹带 = 截断缺失的 {{ formatCount(missingRegisters) }} 个寄存器
                  </div>
                  <!-- 6-bit 打包只是 dense 的事实：sparse 的字节是操作码流，示意图对它是错的 -->
                  <div v-if="inspect.header?.encoding === 'dense'" class="bitwrap">
                    <div class="bs-row"><span>字节 0</span><span>字节 1</span><span>字节 2</span></div>
                    <div class="bitstrip">
                      <i
                        v-for="n in 24"
                        :key="n"
                        :class="[`r${Math.floor((n - 1) / 6)}`, { b8: n === 9 || n === 17 }]"
                      />
                    </div>
                    <div class="bs-row regs">
                      <span>寄存器 0</span><span>1</span><span>2</span><span>3</span>
                    </div>
                    <div class="hx-dim">每 3 字节装 4 个寄存器，边界每 24 bit 才对齐一次 —— hex 在这里天然不可读</div>
                  </div>
                </div>
              </div>
              <div v-else class="hx-dim">
                寄存器无法展开（sparse 操作码损坏或长度异常），请看原始字节。
              </div>
            </div>
          </template>
        </div>

        <!-- 原始：hex + ASCII，只读 -->
        <div v-else class="hxraw">
          <div v-if="rawLoading && !raw" class="emptyline">
            <Icon name="loader" :size="16" />
            <span>正在读取…</span>
          </div>
          <template v-else-if="raw">
            <!-- 与正文完全同字号同结构；染色 span 在原始视图里没有水平 padding，
                 只改背景不改字符步进。空格用插值写死 —— 模板的 whitespace
                 condense 会把标签间的空格删掉或并成一个，pre 布局下列就歪了 -->
            <div v-if="rawHeaderLine" class="hxrow">
              <span class="off">0000</span>
              <span><span class="hxf hxf-magic">{{ rawHeaderLine.magic }}</span>{{ ' ' }}<span class="hxf hxf-enc">{{ rawHeaderLine.enc }}</span>{{ ' ' }}<span class="hxf hxf-resv">{{ rawHeaderLine.resv }}</span>{{ '  ' }}<span class="hxf hxf-card">{{ rawHeaderLine.card }}</span></span>
              <span class="asc">{{ rawHeaderLine.ascii }}</span>
            </div>
            <div v-for="line in hexBody" :key="line.offset" class="hxrow">
              <span class="off">{{ line.offset }}</span>
              <span>{{ line.hex }}</span>
              <span class="asc">{{ line.ascii }}</span>
            </div>
            <div v-if="rawTotal > rawShown" class="hxrow hx-dim">
              <span>… 后续 {{ formatBytes(rawTotal - rawShown) }} 未显示（共 {{ formatBytes(rawTotal) }}）</span>
            </div>
          </template>
        </div>
      </div>
    </div>

    <!-- 状态行：PFCOUNT 的副作用与「不提供保存」都如实写在界面上 -->
    <div class="hll-status">
      <template v-if="corrupted">
        <span v-if="rawTotal > 0" class="num">显示前 {{ formatBytes(rawShown) }} / 共 {{ formatBytes(rawTotal) }}</span>
        <span class="spacer" />
        <span class="hx-dim">查看器不提供保存 —— 改写原始字节请使用 CLI（GET / SET）</span>
      </template>
      <template v-else>
        <span>PFCOUNT · 打开与刷新时执行</span>
        <span class="vsep" />
        <span>计数会刷新服务端头部的 8 字节基数缓存：物理字节可能变化，逻辑值不变</span>
        <span class="spacer" />
        <!-- 只在只读会话渲染：对可写会话陈述「不可添加」是假命题。
             只读时它解释了为什么输入框禁用而基数照常显示 -->
        <span v-if="readonly" class="tag">只读会话：可计数，不可添加</span>
      </template>
    </div>
  </template>
</template>
