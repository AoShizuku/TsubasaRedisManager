import { codecDecode, codecDetect } from '@/api/native'
import { codecLabel, conclusionOf, DETECT_CANDIDATES } from '@/lib/codecs'
import { base64Of, bytesOf } from '@/lib/redisText'
import type { CodecName, RedisText } from '@/types/ipc'
import { javaSerializeCodec } from './java'
import { msgpackCodec } from './msgpack'
import { phpSerializeCodec } from './php'
import { pickleCodec } from './pickle'
import { protobufCodec } from './protobuf'
import type { Decoded, Editable, Node } from './types'

export type { Decoded, Editable, Node } from './types'

/**
 * 渲染进程侧的编解码器：**字节 ↔ 结构**。
 *
 * 只有两个方法，而且 `encode` 不是可选的 —— 往返自检要靠它，见 `checkRoundTrip`。
 * 解不开就抛错，不要返回一个"部分解出来"的结构：半个结构比失败更难发现。
 *
 * 字节 ↔ 字节的那四种压缩算法不走这里，它们归后端（doc/backend/codec.md「归属切分」）。
 */
export interface RendererCodec {
  decode(bytes: Uint8Array): Node
  encode(node: Node): Uint8Array
}

/**
 * 只读编解码器：**字节 → 结构，没有回头路**（`CodecConclusion::Readonly`）。
 *
 * 单独一个接口而不是把 `RendererCodec.encode` 改成可选：可选的 `encode` 意味着
 * 将来任何一个解析器都能靠"不写 encode"绕过往返自检，而自检正是"不列类型清单"
 * 这条设计的全部依托。见 doc/frontend/codecs.md「ReadonlyCodec」。
 *
 * 少了自检就少了解析器的运行时断言，替代物是**消费完整性**
 * （doc/frontend/codec-formats.md「消费完整性」）：
 * 必须吃完整个字节流，并满足格式自己的结束不变量。pickle 要求 STOP 时栈上恰好
 * 一个值；Java 没有全局 STOP，要把 header 后的 0..N 个 content 一直解析到 EOF。
 */
export interface ReadonlyCodec {
  decode(bytes: Uint8Array): Node
  detectPrefix?(bytes: Uint8Array): boolean
}

/**
 * 自动识别时的**额外**前置约束，`detectCodec` 在试 decode 之前先过这一关。
 *
 * 为什么需要第三个维度：`detectable`（`lib/codecs.ts`）说的是"这个格式该不该
 * 进候选集"，而候选集里的判据一直是"decode 抛不抛错"。对 php 那是够的，
 * 对 pickle 不够 —— `N.` 两个字节就是一段合法 pickle（NONE + STOP），
 * 任何以合法 opcode 开头、恰好以 `.` 结尾的短值都会被它认领。
 *
 * 所以 pickle 在识别时额外要求 protocol 2+ 的 `PROTO` 前缀。**手点不受影响**，
 * protocol 0 / 1 照样解得开 —— 这跟 `detectable` 与 `isImplemented` 是两个维度
 * 是同一个道理，见 doc/frontend/codecs.md「识别的三个维度」。
 */
type LocalDecoder = Pick<ReadonlyCodec, 'decode' | 'detectPrefix'>

/**
 * 已实现的渲染进程编解码器。
 *
 * **加一种就是一个新文件加这里一行**，三个解析器是同一个形状：模块只依赖
 * `./types`（外加 `import type { RendererCodec }`，类型导入会被完全擦掉，
 * 不构成循环依赖），对外导出一个 codec 常量，挂载写在这张表上。
 *
 * 注册集中在表上，而不是让各模块 import 时自注册：后者会引入"没人 import 它就
 * 没注册"的启动顺序问题，而这张表恰恰是 `isImplemented()` 的唯一依据 ——
 * 界面亮不亮全看它。表里没有的那几种，界面上一律置灰。
 */
export const RENDERER_CODECS: Partial<Record<CodecName, RendererCodec>> = {
  msgpack: msgpackCodec,
  phpSerialize: phpSerializeCodec,
  protobuf: protobufCodec,
}

/**
 * 已实现的只读编解码器。**这张表与上面那张不能合并**，见 `ReadonlyCodec`。
 *
 * `conclusionOf(name) === 'readonly'` 的那些只查这张表；反过来，往
 * `RENDERER_CODECS` 里挂一个 readonly 的实现**不算数**（`isImplemented` 按
 * conclusion 分派，不是两张表都翻一遍）。`codec.test.ts` 钉着这一条。
 */
export const READONLY_CODECS: Partial<Record<CodecName, ReadonlyCodec>> = {
  pickle: pickleCodec,
  javaSerialize: javaSerializeCodec,
}

/**
 * 只读类的 `Editable`，一个常量。
 *
 * **不走 `checkRoundTrip`** —— 那需要 `encode`，而这一类的定义就是没有 encode。
 * `kind: 'noEncoder'` 让界面说对话：pickle 不是"编不回原字节"（它编得回去），
 * 是我们**不给**它写编码器。理由是两条，都在 doc/backend/codec.md「Readonly 安全边界」：
 * 写回等于制造一个会被别人加载的可执行载荷；以及 memo 编号依赖序列化时的对象共享，
 * 跟 PHP 的 `R:` 同构。
 */
function readonlyEditable(name: CodecName): Editable {
  return {
    ok: false,
    kind: 'noEncoder',
    reason: `${codecLabel(name)} 只解码不编码：写回一个${codecLabel(name)}值等于制造一份会被别的程序加载并执行的载荷`,
  }
}

/**
 * 此刻能不能真的跑。
 *
 * **这是"能不能跑"，不是"能不能自动识别"** —— 后者是 `CodecInfo.detectable`，
 * 两个维度独立，见 doc/frontend/codecs.md「识别的三个维度」。
 *
 * 不写死成一张表：压缩类由后端保证一直可用，序列化类跟着注册表走，
 * 于是加一个解析器只要往 `RENDERER_CODECS` 里挂，界面自己就亮了。
 */
export function isImplemented(name: CodecName): boolean {
  const conclusion = conclusionOf(name)
  if (conclusion === 'rustRoundTrip') return true
  if (conclusion === 'rendererRoundTrip') return name in RENDERER_CODECS
  // **按 conclusion 分派到对应的那一张表**，不是两张都翻一遍：readonly 的实现
  // 挂进 RENDERER_CODECS 不该让它变成可用，那张表的契约含 encode
  if (conclusion === 'readonly') return name in READONLY_CODECS
  return false
}

/**
 * 置灰时给出的理由。界面要照实说，不要等用户点下去才报错。
 *
 * `readonly` 的措辞要小心：安全边界禁的是**完整支持**（反序列化即执行，
 * pickle 的 `__reduce__` 与 Java 的 `readObject` 都能被构造成 RCE），
 * 不是禁解析本身。不实例化任何对象的只读结构解析是允许的 —— pickle 与
 * `javaSerialize` 都是这么做的（doc/backend/codec.md「Readonly 安全边界」）。
 * 说成"刻意不做"会让后人以为 `CodecConclusion::Readonly` 等于禁区 ——
 * 那样这个 conclusion 就没有存在的意义了，直接叫 Forbidden 就好。
 */
export function unimplementedReason(name: CodecName): string | null {
  if (isImplemented(name)) return null
  const conclusion = conclusionOf(name)
  if (conclusion === 'readonly') {
    return `${codecLabel(name)} 只能做不实例化对象的只读解析（反序列化即执行），该解析器还没有实现`
  }
  return `${codecLabel(name)} 的解析器还没有实现`
}

/**
 * 解码的统一入口，按 `conclusion` 分派。
 *
 *   rustRoundTrip     走 IPC，产物是字节（`RedisText`）
 *   rendererRoundTrip 查注册表，产物是结构（`Node`），跟一次往返自检
 *   readonly          查另一张注册表，产物是结构，**不自检**（没有 encode）
 *   其余              没有实现，抛错
 *
 * **截断守卫不在这里。** 它在 `CodecPicker` 层：拒绝之后还要给得出下一步
 * （"加载完整值"），给不出时还要说清为什么给不出，这些只有界面知道。
 * 见 doc/frontend/codecs.md「截断守卫」。
 */
export async function decodeValue(codec: CodecName, source: RedisText): Promise<Decoded> {
  const conclusion = conclusionOf(codec)

  if (conclusion === 'rustRoundTrip') {
    const result = await codecDecode({ codec, inputBase64: base64Of(source) })
    return { kind: 'text', value: result.value }
  }

  const impl = RENDERER_CODECS[codec]
  if (conclusion === 'rendererRoundTrip' && impl) {
    return decodeWithRenderer(impl, bytesOf(source))
  }

  const readonlyImpl = READONLY_CODECS[codec]
  if (conclusion === 'readonly' && readonlyImpl) {
    return decodeWithReadonly(readonlyImpl, bytesOf(source), codec)
  }

  throw new Error(unimplementedReason(codec) ?? `${codecLabel(codec)} 无法解码`)
}

/** 解码 + 往返自检。解不开时 `decode` 抛出的错原样往上传。 */
export function decodeWithRenderer(impl: RendererCodec, bytes: Uint8Array): Decoded {
  const root = impl.decode(bytes)
  return { kind: 'struct', root, editable: checkRoundTrip(impl, root, bytes) }
}

/**
 * 只读解码。产物形状与 `decodeWithRenderer` 完全一致 —— 界面那一侧不必知道
 * 这个值是从哪张表来的，它只看 `editable`。
 *
 * 这里**没有**自检可做，所以解析器自身的正确性全靠消费完整性
 * （doc/frontend/codec-formats.md「消费完整性」）：
 * 吃完整个字节流、满足各格式自己的结束不变量，都在解析器内部保证，违反就抛错。
 * 这也是自动识别的判据 —— 宽松的解析器会连带毁掉它。
 */
export function decodeWithReadonly(
  impl: ReadonlyCodec,
  bytes: Uint8Array,
  name: CodecName,
): Decoded {
  return { kind: 'struct', root: impl.decode(bytes), editable: readonlyEditable(name) }
}

/**
 * 往返自检：解出结构后立刻 re-encode，与原字节**逐字节**比对。
 *
 * **不要改成"列出哪些类型可往返"** —— 那张清单必然漏。msgpack 的 str/bin 歧义、
 * float32 vs float64、map 顺序、protobuf 的 varint 冗余编码与 packed repeated、
 * PHP 的 `R:` / `r:` 引用编号（依赖序列化时的遍历顺序，编辑后重建会指错）、
 * `C:` 的内容是类自己写的原始字节而根本不自描述 —— 每一条都是坑。
 * 机械验证一次全覆盖，而且失败时能给出**具体**理由而不是一句"不支持"。
 *
 * 见 doc/frontend/codecs.md「往返自检」。
 */
export function checkRoundTrip(
  impl: RendererCodec,
  root: Node,
  original: Uint8Array,
): Editable {
  let reencoded: Uint8Array
  try {
    reencoded = impl.encode(root)
  } catch (error) {
    return {
      ok: false,
      kind: 'lossy',
      reason: `重新编码失败：${error instanceof Error ? error.message : String(error)}`,
    }
  }

  if (reencoded.length !== original.length) {
    return {
      ok: false,
      kind: 'lossy',
      reason: `重新编码后长度对不上（原 ${original.length} 字节，重编 ${reencoded.length} 字节）`,
    }
  }

  for (let i = 0; i < original.length; i += 1) {
    if (reencoded[i] !== original[i]) {
      return { ok: false, kind: 'lossy', reason: `重新编码后第 ${i + 1} 个字节对不上` }
    }
  }

  return { ok: true }
}

/**
 * 本地解析器，**按 conclusion 选表**。
 *
 * 与 `isImplemented` 共用同一条分派规则，两处就不会走散 —— 尤其是
 * "readonly 的实现挂进 `RENDERER_CODECS` 不算数"这一条，靠 `??` 兜底会破功。
 */
function localDecoderFor(name: CodecName): LocalDecoder | undefined {
  const conclusion = conclusionOf(name)
  if (conclusion === 'rendererRoundTrip') return RENDERER_CODECS[name]
  if (conclusion === 'readonly') return READONLY_CODECS[name]
  return undefined
}

/**
 * 自动识别。两趟：先在渲染进程本地试，剩下的交给后端。
 *
 * 本地这一趟是必需的：后端的 `detect` 拿"能不能解开"当判据，而它根本没有
 * 序列化类的实现，phpSerialize 与 pickle 送过去只会被跳过。
 * 候选集来自 `DETECT_CANDIDATES`（跟着 `detectable` 走），后端会自己忽略
 * 它跑不了的那些，所以这里原样传过去就行。
 */
export async function detectCodec(source: RedisText): Promise<CodecName | null> {
  const bytes = bytesOf(source)

  for (const name of DETECT_CANDIDATES) {
    const impl = localDecoderFor(name)
    if (!impl) continue
    // 有额外前置约束的先过那一关，见 `ReadonlyCodec.detectPrefix`
    if (impl.detectPrefix && !impl.detectPrefix(bytes)) continue
    try {
      impl.decode(bytes)
      return name
    } catch {
      // 解不开就是不是它。能进候选集的都有可靠前缀，所以这一试失败得很快。
    }
  }

  const result = await codecDetect({
    inputBase64: base64Of(source),
    candidates: DETECT_CANDIDATES,
  })
  return result.codec
}
