import type { CodecConclusion, CodecName } from '@/types/ipc'

/**
 * 编解码器能力表，与 Rust 的 codec_support_matrix() 一一对应。
 *
 * 这里只记**静态**的两件事：归属（`conclusion`）与能不能自动识别（`detectable`）。
 * "此刻能不能跑"是第三件事，由 conclusion 加渲染进程注册表算出来，
 * 所以它在 `lib/codec/index.ts` 的 `isImplemented()` 里，不在这张表上 ——
 * 表里放一个会跟着注册表变的字段，只会让两处不同步。
 */
export interface CodecInfo {
  name: CodecName
  label: string
  conclusion: CodecConclusion
  /**
   * 能不能进自动识别的候选。
   *
   * **这与"能不能跑"是两个独立维度，不要让它跟着后者走。** 判据只有一条：
   * 这个格式有没有可靠的前缀 / 魔数。没有的话，进了候选就会把一切都识别成它。
   *
   *   gzip / deflate / deflateRaw   有魔数，而且是"试解成功才算"，误判代价低
   *   brotli                        无魔数，但试解会失败得很快，留着当兜底
   *   phpSerialize                  `a:` / `O:` / `s:` / `i:` / `d:` / `b:` / `N;` 足够可靠
   *   msgpack / protobuf            **永远不能进** —— 无魔数，任意字节都可能是合法
   *                                 msgpack 或合法 protobuf，进了候选之后
   *                                 每一个值都会被识别成它，自动识别就废了
   *   pickle                        `80 02`..`80 05` 的 PROTO 前缀，加上"必须以
   *                                 STOP 结尾且吃完全部字节"这两条硬约束，够可靠。
   *                                 **只覆盖 protocol 2+** —— 0 / 1 没有 PROTO
   *                                 opcode，前缀是任意 opcode 字节，放宽会重蹈
   *                                 msgpack 那条覆辙。手点仍然解得开
   *   javaSerialize                 `ac ed 00 05` 是完整 stream header；解析器还会
   *                                 继续严格消费到输入末尾，可靠且可以自动识别
   *
   * 见 doc/frontend/codecs.md「识别的三个维度」与 doc/backend/codec.md「自动识别的边界」。
   */
  detectable: boolean
}

export const CODECS: CodecInfo[] = [
  { name: 'gzip', label: 'GZIP', conclusion: 'rustRoundTrip', detectable: true },
  { name: 'deflate', label: 'Deflate (zlib)', conclusion: 'rustRoundTrip', detectable: true },
  { name: 'deflateRaw', label: 'Deflate (raw)', conclusion: 'rustRoundTrip', detectable: true },
  { name: 'brotli', label: 'Brotli', conclusion: 'rustRoundTrip', detectable: true },
  { name: 'msgpack', label: 'MessagePack', conclusion: 'rendererRoundTrip', detectable: false },
  { name: 'phpSerialize', label: 'PHP serialize', conclusion: 'rendererRoundTrip', detectable: true },
  { name: 'protobuf', label: 'Protobuf', conclusion: 'rendererRoundTrip', detectable: false },
  { name: 'pickle', label: 'Python pickle', conclusion: 'readonly', detectable: true },
  { name: 'javaSerialize', label: 'Java 序列化', conclusion: 'readonly', detectable: true },
]

/**
 * 归属不是用户关心的事，所以文案里不出现"渲染进程"——
 * 界面只该说这个编解码器能做什么，不该说它在哪一侧做。
 */
export const CONCLUSION_LABEL: Record<CodecConclusion, string> = {
  rustRoundTrip: '可解可编',
  rendererRoundTrip: '可解可编',
  readonly: '只能解码',
  deferredUnsupported: '暂不支持',
}

/**
 * 自动识别的候选集，跟着 `detectable` 走。
 *
 * **不要改回跟着"能不能跑"走。** 那样每实现一个新解析器候选集就自动变长，
 * 而 msgpack / protobuf 恰恰是实现了也不能进的那两个。
 */
export const DETECT_CANDIDATES: CodecName[] = CODECS.filter((codec) => codec.detectable).map(
  (codec) => codec.name,
)

export function codecLabel(name: CodecName): string {
  return CODECS.find((codec) => codec.name === name)?.label ?? name
}

export function conclusionOf(name: CodecName): CodecConclusion {
  return CODECS.find((codec) => codec.name === name)?.conclusion ?? 'deferredUnsupported'
}
