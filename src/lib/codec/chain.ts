import { codecEncode } from '@/api/native'
import { codecLabel, conclusionOf } from '@/lib/codecs'
import { formatBytes } from '@/lib/format'
import { base64FromBytes, bytesFromBase64, bytesOf, isTruncated, textFromBytes } from '@/lib/redisText'
import type { CodecName, RedisText } from '@/types/ipc'
import { decodeValue } from './index'
import type { Decoded } from './types'

/**
 * 链式解码：**`gzip → msgpack` 那条用例**。
 *
 * `CodecDecodeResult` 给的是字节而不是文本，理由就是它：解压的产物是字节，
 * 字节上还能再套一层。界面要是只记得住**一个** codec，每次送去解码的就都还是
 * 原始字节 —— 解完 gzip 再点 msgpack，送过去的仍然是 gzip 字节。
 * 见 doc/frontend/codecs.md「链」。
 *
 * 链是**不可变**的，每次操作返回新对象：`Node` 是递归类型，装它的 ref 一律
 * `shallowRef`（`ref` 会让 Vue 的 `UnwrapRef` 把 TS 类型实例化撑爆，TS2589），
 * 而 `shallowRef` 只认整块替换。
 *
 * 逻辑放在这里而不是组件里，是因为它要被测：正向每一步喂上一步的产物、
 * 反向 encode 的顺序、struct 之后不能续链、截断在链的中途被拦下 ——
 * 这四条都是纯字节的事，不该只能靠点界面来验。
 */

/** 链上的一步：用了哪个 codec，解出来什么。 */
export interface ChainStep {
  codec: CodecName
  /**
   * 这一步的产物。**`kind: 'struct'` 的那一步一定是最后一步** ——
   * `Node` 不是字节，套不了下一层 codec，见 `isTerminal`。
   */
  result: Decoded
}

export interface CodecChain {
  /** 第 0 步的输入：从 Redis 读回来的那份字节。"还原原始值"回到的就是它。 */
  source: RedisText
  steps: readonly ChainStep[]
}

export type DecodedStruct = Extract<Decoded, { kind: 'struct' }>

export function newChain(source: RedisText): CodecChain {
  return { source, steps: [] }
}

export function appliedCodecs(chain: CodecChain): CodecName[] {
  return chain.steps.map((step) => step.codec)
}

/** `GZIP → MessagePack`。空链返回空串，调用方自己决定那时显示什么。 */
export function chainLabel(chain: CodecChain): string {
  return chain.steps.map((step) => codecLabel(step.codec)).join(' → ')
}

/**
 * 链上当前这一层的**字节**。
 *
 * struct 结尾时给的是那一步的**输入**（结构不是字节，拿不出来）——
 * 屏幕上显示的是结构，但"这个值有多少字节"问的仍然是这一份。
 */
export function chainBytes(chain: CodecChain): RedisText {
  for (let i = chain.steps.length - 1; i >= 0; i -= 1) {
    const result = chain.steps[i].result
    if (result.kind === 'text') return result.value
  }
  return chain.source
}

/** 链尾解出来的结构，没有就是 `null`。 */
export function chainStruct(chain: CodecChain): DecodedStruct | null {
  const last = chain.steps[chain.steps.length - 1]
  return last && last.result.kind === 'struct' ? last.result : null
}

/** 链到头了：解出结构之后接不上下一步。 */
export function isTerminal(chain: CodecChain): boolean {
  return chainStruct(chain) !== null
}

export const TERMINAL_REASON =
  '已经解出结构，结构不是字节，不能再套一层编解码器。要换一种解码，先撤销这一步。'

/**
 * 截断守卫的文案。**半截 msgpack 跟半截 gzip 一样解不开**，两类一起拦。
 *
 * 不只第一步，链的**每一步**都要守：这里同时是 `extendChain` 的
 * 拒绝理由与菜单上的置灰理由，两处共用一份措辞就不会走散。
 * 见 doc/frontend/codecs.md「截断守卫」。
 */
export function truncatedReason(value: RedisText): string {
  const shown = formatBytes(bytesOf(value).length)
  const total = formatBytes(value.totalBytes ?? 0)
  return `值已截断（前 ${shown} / 共 ${total}），压缩与序列化格式都要完整字节才解得开`
}

/**
 * 为什么接不上下一步，`null` = 接得上。
 *
 * `input` 是这一步真正要解的字节，默认取链尾。编辑器里改过之后解码的是**屏幕上
 * 那份**，所以调用方可以传进来 —— 链只记录用过哪些 codec，反向 encode 是对
 * 这张 codec 表求逆，中途改过字节不影响它成立。
 */
export function extendBlockedReason(chain: CodecChain, input?: RedisText): string | null {
  if (isTerminal(chain)) return TERMINAL_REASON
  const value = input ?? chainBytes(chain)
  if (isTruncated(value)) return truncatedReason(value)
  return null
}

/** 往链上接一步。被守卫拦下时抛错，而不是悄悄返回原链。 */
export async function extendChain(
  chain: CodecChain,
  codec: CodecName,
  input?: RedisText,
): Promise<CodecChain> {
  const blocked = extendBlockedReason(chain, input)
  if (blocked) throw new Error(blocked)
  const result = await decodeValue(codec, input ?? chainBytes(chain))
  return { source: chain.source, steps: [...chain.steps, { codec, result }] }
}

/** 退一步。解错 codec 是常事，没有这条路只能重开标签页。 */
export function undoStep(chain: CodecChain): CodecChain {
  return { source: chain.source, steps: chain.steps.slice(0, -1) }
}

/** 一路退回原始值。 */
export function resetChain(chain: CodecChain): CodecChain {
  return { source: chain.source, steps: [] }
}

/* ── 反向 encode ─────────────────────────────────────── */

export const STRUCT_LOSSY_PREFIX = '解出来的结构编不回原来的字节'

/**
 * 为什么不能保存，`null` = 可以保存。
 *
 * **压缩类的链是可以保存的**（gzip 解开 → 改 JSON → 存回去仍然是 gzip），
 * 那正是最常见的真实用例，不要连它一起锁掉。挡下来的只有 struct 结尾，
 * 而且两种挡法是**不同性质**的两件事，文案不能混：
 *
 *   - `kind: 'lossy'` —— **数据的性质**：这个值重编回去就不是原字节了
 *     （PHP 的 `R:` / `C:`、msgpack 的非最短编码、protobuf 的非最短 varint）
 *   - `kind: 'noEncoder'` —— **格式的性质**：我们不给这个格式写编码器。
 *     注意它跟上一条**处境相反** —— pickle 编得回去，是我们不编，见
 *     doc/backend/codec.md「Readonly 安全边界」。套 `STRUCT_LOSSY_PREFIX` 就成了
 *     假话，让用户以为是他的数据有问题
 *   - `editable.ok === true` —— **工具的能力**：结构是完整可逆的，缺的是
 *     结构编辑器。`render.ts` 产出的是带类型标注的展示文本（`i64 123` / `~msg`
 *     / `bytes(4)`），**它不是一个能解析回 `Node` 的格式**，也不该是 ——
 *     那等于把结构压成文本再读回来。所以结构没有编辑路径。
 *     这一条要照实说，别让用户以为是他的数据有问题。
 *
 * 三种说法见 doc/frontend/codecs.md「Editable 的三种说法」。
 */
export function saveBlockedReason(chain: CodecChain): string | null {
  const struct = chainStruct(chain)
  if (!struct) return null
  if (!struct.editable.ok) {
    if (struct.editable.kind === 'noEncoder') {
      return `${struct.editable.reason}。这个值本身没有问题，能解开看，但本工具不提供写回这一步。`
    }
    return `${STRUCT_LOSSY_PREFIX}（${struct.editable.reason}），照这份呈现保存会改写这个值。`
  }
  return '这个值的结构完整可逆，数据本身没有问题；缺的是结构编辑器 —— 编辑器里是带类型标注的展示文本，不是能读回结构的格式，照着它保存会写坏这个值。'
}

/**
 * 单步 encode。压缩类走后端 —— 这是 `native_codec_encode` 的**第一个调用者**，
 * 它从加进来那天起就没人调过。
 *
 * 序列化类走不到这里：解出结构的那一步一定是链尾，而链尾有结构时保存被
 * `saveBlockedReason` 挡住。将来有了结构编辑器，那条路是
 * `RENDERER_CODECS[codec].encode(node)` 拿到字节，再把**前缀**那几层交给
 * `encodeAlongChain` —— 那三个 `encode` 实现现在的作用是往返自检本身
 * （判定"这个值能不能安全写回"），到时候直接就能用。
 */
async function encodeStep(codec: CodecName, bytes: Uint8Array): Promise<Uint8Array> {
  if (conclusionOf(codec) !== 'rustRoundTrip') {
    throw new Error(`${codecLabel(codec)} 还没有结构编辑器，这一层编不回去`)
  }
  const result = await codecEncode({ codec, inputBase64: base64FromBytes(bytes) })
  return bytesFromBase64(result.outputBase64)
}

/**
 * 沿链**反向** encode，返回**每一层**的字节。
 *
 * `[0]` 是最外层（写回 Redis 的那份），末位就是传进来的 `tail`，
 * `levels[i]` 是 `codecs[i]` 的输入。顺序反了就是一次静默的数据损坏，
 * 所以把中间层一起返回：既让调用方能把链上的字节换成刚写进去的那份
 * （见 `rebaseChain`），也让顺序这件事**可以被断言**，不必靠读代码确认。
 *
 * 逐层 await 是必须的：每一层的输入是上一层的输出，并行不了。
 */
export async function encodeAlongChain(
  codecs: readonly CodecName[],
  tail: Uint8Array,
): Promise<Uint8Array[]> {
  const levels: Uint8Array[] = new Array<Uint8Array>(codecs.length + 1)
  levels[codecs.length] = tail
  for (let i = codecs.length - 1; i >= 0; i -= 1) {
    levels[i] = await encodeStep(codecs[i], levels[i + 1])
  }
  return levels
}

/**
 * 保存之后把链上的字节换成**刚写进去的那份**。
 *
 * 不做这一步，"还原原始值"会掏出一份已经不在 Redis 里的旧值 ——
 * 界面会说谎，而且说的是"这就是库里的东西"这种最难被怀疑的谎。
 * `levels` 正好是每一层的新字节，重新解码一遍就没必要了。
 */
export function rebaseChain(chain: CodecChain, levels: Uint8Array[]): CodecChain {
  return {
    source: textFromBytes(levels[0]),
    steps: chain.steps.map((step, index) => ({
      codec: step.codec,
      result: { kind: 'text', value: textFromBytes(levels[index + 1]) },
    })),
  }
}
