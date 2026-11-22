import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  appliedCodecs,
  chainBytes,
  chainLabel,
  chainStruct,
  encodeAlongChain,
  extendBlockedReason,
  extendChain,
  isTerminal,
  newChain,
  rebaseChain,
  resetChain,
  saveBlockedReason,
  TERMINAL_REASON,
  undoStep,
  type CodecChain,
} from './chain'
import { RENDERER_CODECS, type RendererCodec } from './index'
import { base64FromBytes, bytesFromBase64, bytesOf, textFromBytes } from '@/lib/redisText'
import type { CodecName, RedisText } from '@/types/ipc'

/**
 * 压缩类的真实算法归 Rust，这一层要验的是**链**：每一步喂的是不是上一步的产物、
 * 反向 encode 的顺序、struct 之后能不能续、截断有没有被拦下。
 *
 * 所以假后端用的是一对精确互逆、而且**不对称**的变换：encode 前置一个魔数字节，
 * decode 认这个字节再剥掉。顺序反了立刻能看出来 —— 真的 gzip / deflate 反过来
 * 套一样能编出一坨字节，只是解不开，那种失败要跑到用户那里才发现。
 */
const MAGIC: Partial<Record<CodecName, number>> = {
  gzip: 0x67,
  deflate: 0x64,
  brotli: 0x62,
}

const native = vi.hoisted(() => ({
  codecDecode: vi.fn(),
  codecEncode: vi.fn(),
  codecDetect: vi.fn(),
}))
vi.mock('@/api/native', () => native)

/** 忠实往返：整块字节进一个 bytes 节点，再原样编回去 → 自检通过。 */
const faithful: RendererCodec = {
  decode: (bytes) => ({ t: 'bytes', v: bytes }),
  encode: (node) => (node.t === 'bytes' ? node.v : new Uint8Array()),
}

/** 编回去少一个字节，像 msgpack 的非最短编码 → 自检不过。 */
const lossy: RendererCodec = {
  decode: (bytes) => ({ t: 'bytes', v: bytes }),
  encode: (node) => (node.t === 'bytes' ? node.v.slice(1) : new Uint8Array()),
}

const original = RENDERER_CODECS.msgpack

beforeEach(() => {
  RENDERER_CODECS.msgpack = faithful
  native.codecEncode.mockImplementation(({ codec, inputBase64 }: EncodeCall) => {
    const bytes = bytesFromBase64(inputBase64)
    const out = new Uint8Array(bytes.length + 1)
    out[0] = MAGIC[codec] ?? 0
    out.set(bytes, 1)
    return Promise.resolve({ outputBase64: base64FromBytes(out) })
  })
  native.codecDecode.mockImplementation(({ codec, inputBase64 }: EncodeCall) => {
    const bytes = bytesFromBase64(inputBase64)
    if (bytes[0] !== MAGIC[codec]) return Promise.reject(new Error('不是这个格式'))
    return Promise.resolve({ value: textFromBytes(bytes.slice(1)) })
  })
})

afterEach(() => {
  RENDERER_CODECS.msgpack = original
  vi.resetAllMocks()
})

interface EncodeCall {
  codec: CodecName
  inputBase64: string
}

function bytes(...values: number[]): Uint8Array {
  return Uint8Array.from(values)
}

/** `gzip(deflate(text))`，两层压缩包一段 JSON —— 链式解码那条用例的形状。 */
function packed(text: string): RedisText {
  const body = new TextEncoder().encode(text)
  const out = new Uint8Array(body.length + 2)
  out[0] = MAGIC.gzip ?? 0
  out[1] = MAGIC.deflate ?? 0
  out.set(body, 2)
  return textFromBytes(out)
}

describe('正向：每一步的输入是上一步的输出', () => {
  it('两层压缩一层层剥开', async () => {
    let chain = newChain(packed('{"a":1}'))

    chain = await extendChain(chain, 'gzip')
    expect(chainBytes(chain).text).toBe(`${String.fromCharCode(MAGIC.deflate ?? 0)}{"a":1}`)

    chain = await extendChain(chain, 'deflate')
    expect(chainBytes(chain).text).toBe('{"a":1}')
    expect(appliedCodecs(chain)).toEqual(['gzip', 'deflate'])
    expect(chainLabel(chain)).toBe('GZIP → Deflate (zlib)')
  })

  /* 链存在的理由（doc/frontend/codecs.md「链」）：第二步送过去的要是还是原始字节，
     gzip 里裹着的 msgpack 就永远解不开。 */
  it('第二步送去后端的是第一步的产物，不是原始字节', async () => {
    const chain = await extendChain(newChain(packed('hi')), 'gzip')
    await extendChain(chain, 'deflate')

    const [first, second] = native.codecDecode.mock.calls.map((call) => call[0] as EncodeCall)
    expect(bytesOf(packed('hi'))).toEqual(bytes(MAGIC.gzip ?? 0, MAGIC.deflate ?? 0, 104, 105))
    expect(bytesFromBase64(first.inputBase64)).toEqual(
      bytes(MAGIC.gzip ?? 0, MAGIC.deflate ?? 0, 104, 105),
    )
    expect(bytesFromBase64(second.inputBase64)).toEqual(bytes(MAGIC.deflate ?? 0, 104, 105))
  })

  it('解不开时抛错，链保持原样', async () => {
    const chain = newChain(packed('hi'))
    await expect(extendChain(chain, 'brotli')).rejects.toThrow('不是这个格式')
    expect(chain.steps).toHaveLength(0)
  })

  it('压缩类之后接序列化类，解出的是结构', async () => {
    let chain = await extendChain(newChain(packed('hi')), 'gzip')
    chain = await extendChain(chain, 'msgpack')

    expect(chainStruct(chain)?.root).toEqual({
      t: 'bytes',
      v: bytes(MAGIC.deflate ?? 0, 104, 105),
    })
    // 结构那一步不是字节，"当前字节"给的是它的输入
    expect(bytesOf(chainBytes(chain))).toEqual(bytes(MAGIC.deflate ?? 0, 104, 105))
  })
})

describe('struct 之后链就到头了', () => {
  it('接不上下一步，理由说在菜单上而不是点下去才报错', async () => {
    const chain = await extendChain(newChain(textFromBytes(bytes(1, 2, 3))), 'msgpack')

    expect(isTerminal(chain)).toBe(true)
    expect(extendBlockedReason(chain)).toBe(TERMINAL_REASON)
    await expect(extendChain(chain, 'gzip')).rejects.toThrow(TERMINAL_REASON)
    expect(native.codecDecode).not.toHaveBeenCalled()
  })

  it('撤销之后又能接了', async () => {
    const chain = await extendChain(newChain(packed('hi')), 'gzip')
    const terminal = await extendChain(chain, 'msgpack')

    expect(extendBlockedReason(undoStep(terminal))).toBeNull()
  })
})

describe('回退', () => {
  it('退一步回到上一层的字节，一路退回原始值', async () => {
    const source = packed('{"a":1}')
    let chain = await extendChain(newChain(source), 'gzip')
    chain = await extendChain(chain, 'deflate')

    const back = undoStep(chain)
    expect(appliedCodecs(back)).toEqual(['gzip'])
    expect(chainBytes(back).text).toBe(`${String.fromCharCode(MAGIC.deflate ?? 0)}{"a":1}`)

    expect(appliedCodecs(resetChain(chain))).toEqual([])
    expect(chainBytes(resetChain(chain))).toEqual(source)
  })

  it('空链上退一步是无操作，不是负数长度', () => {
    const chain = newChain(packed('hi'))
    expect(undoStep(chain).steps).toHaveLength(0)
  })

  /* 回退不重解一遍：解码可能要走 IPC，退一步却应该是即时的。 */
  it('回退不发第二次解码请求', async () => {
    const chain = await extendChain(newChain(packed('hi')), 'gzip')
    native.codecDecode.mockClear()

    undoStep(chain)
    resetChain(chain)
    expect(native.codecDecode).not.toHaveBeenCalled()
  })
})

describe('截断守卫', () => {
  const cut: RedisText = { text: 'half a gzip', base64: null, totalBytes: 999999 }

  it('第一步就拦下', async () => {
    const chain = newChain(cut)
    expect(extendBlockedReason(chain)).toContain('值已截断')
    await expect(extendChain(chain, 'gzip')).rejects.toThrow('值已截断')
  })

  /* 不只第一步，链的每一步都要守 —— 中途冒出来的截断值跟第一步的截断值
     一样解不开。见 doc/frontend/codecs.md「截断守卫」。 */
  it('链的中途也拦下', async () => {
    native.codecDecode.mockResolvedValueOnce({ value: cut })
    const chain = await extendChain(newChain(packed('hi')), 'gzip')

    expect(extendBlockedReason(chain)).toContain('值已截断')
    await expect(extendChain(chain, 'deflate')).rejects.toThrow('值已截断')
  })

  /* 编辑器里改过之后解的是屏幕上那份，守卫要守的也是那份。 */
  it('守的是真正要解的那份字节，而不只是链尾', () => {
    const chain = newChain(packed('hi'))
    expect(extendBlockedReason(chain, cut)).toContain('值已截断')
  })
})

describe('反向 encode', () => {
  it('多层链逐层编回去，顺序不能反', async () => {
    const tail = new TextEncoder().encode('{"a":1}')
    const levels = await encodeAlongChain(['gzip', 'deflate'], tail)

    expect(levels).toHaveLength(3)
    expect(levels[2]).toEqual(tail)
    // deflate 先编（它离数据近），gzip 后编（它在最外层）
    expect(levels[1][0]).toBe(MAGIC.deflate)
    expect(levels[0][0]).toBe(MAGIC.gzip)
    expect(levels[0][1]).toBe(MAGIC.deflate)

    const order = native.codecEncode.mock.calls.map((call) => (call[0] as EncodeCall).codec)
    expect(order).toEqual(['deflate', 'gzip'])
  })

  it('编回去的字节能原样再解开', async () => {
    const text = '{"a":1}'
    const levels = await encodeAlongChain(['gzip', 'deflate'], new TextEncoder().encode(text))

    let chain = newChain(textFromBytes(levels[0]))
    chain = await extendChain(chain, 'gzip')
    chain = await extendChain(chain, 'deflate')
    expect(chainBytes(chain).text).toBe(text)
  })

  it('空链原样返回，保存路径退化成"照写"', async () => {
    const tail = new TextEncoder().encode('plain')
    const levels = await encodeAlongChain([], tail)

    expect(levels).toEqual([tail])
    expect(native.codecEncode).not.toHaveBeenCalled()
  })

  /* 序列化类走不到这里（struct 一定是链尾，而链尾有结构时保存被挡住），
     但真走到了要说清是"还没有结构编辑器"，不是"编码失败"。 */
  it('序列化类这一层编不回去，理由是缺编辑器', async () => {
    await expect(encodeAlongChain(['msgpack'], bytes(1))).rejects.toThrow('结构编辑器')
    expect(native.codecEncode).not.toHaveBeenCalled()
  })
})

describe('保存后把链上的字节换成刚写进去的那份', () => {
  it('每一层都换掉，"还原原始值"不会掏出旧值', async () => {
    let chain = await extendChain(newChain(packed('old')), 'gzip')
    chain = await extendChain(chain, 'deflate')

    const levels = await encodeAlongChain(appliedCodecs(chain), new TextEncoder().encode('new'))
    const rebased = rebaseChain(chain, levels)

    expect(appliedCodecs(rebased)).toEqual(['gzip', 'deflate'])
    expect(chainBytes(rebased).text).toBe('new')
    expect(bytesOf(rebased.source)).toEqual(levels[0])
    expect(chainBytes(resetChain(rebased)).text).toBe(
      `${String.fromCharCode(MAGIC.gzip ?? 0)}${String.fromCharCode(MAGIC.deflate ?? 0)}new`,
    )
  })

  /* 一律填 base64 的话，一份 JSON 回退一步会被 isBinaryText 判成二进制、
     显示成十六进制 —— 所以 textFromBytes 要真的验一遍 UTF-8。 */
  it('UTF-8 的那几层还是文本，不是十六进制', async () => {
    const chain = await extendChain(newChain(packed('{"a":1}')), 'gzip')
    const levels = await encodeAlongChain(['gzip'], new TextEncoder().encode('{"a":1}'))
    const rebased = rebaseChain(chain, levels)

    expect(chainBytes(rebased).text).toBe('{"a":1}')
    expect(chainBytes(rebased).base64).toBeNull()
    // 最外层带着魔数字节，仍然是合法 UTF-8，所以也走 text —— 判据是字节不是层数
    expect(rebased.source.text).not.toBeNull()
  })
})

describe('为什么不能保存', () => {
  it('压缩类的链可以保存 —— 这是最常见的真实用例，别一起锁掉', async () => {
    const chain = await extendChain(newChain(packed('{"a":1}')), 'gzip')
    expect(saveBlockedReason(chain)).toBeNull()
    expect(saveBlockedReason(newChain(packed('x')))).toBeNull()
  })

  /* 两种失败是不同性质的事，文案不能混：一个是数据的性质，一个是工具的能力。 */
  it('自检不过说的是数据编不回原字节', async () => {
    RENDERER_CODECS.msgpack = lossy
    const chain = await extendChain(newChain(textFromBytes(bytes(1, 2, 3))), 'msgpack')

    expect(saveBlockedReason(chain)).toContain('编不回原来的字节')
  })

  it('自检通过但仍然只读时，要说清是缺编辑器而不是数据有问题', async () => {
    const chain = await extendChain(newChain(textFromBytes(bytes(1, 2, 3))), 'msgpack')
    const reason = saveBlockedReason(chain) ?? ''

    expect(chainStruct(chain)?.editable.ok).toBe(true)
    expect(reason).toContain('数据本身没有问题')
    expect(reason).toContain('结构编辑器')
    expect(reason).not.toContain('编不回原来的字节')
  })
})

describe('链是不可变的', () => {
  /* 组件里装它的是 shallowRef（`Node` 是递归类型，`ref` 会把 TS 撑爆），
     而 shallowRef 只认整块替换 —— 原地改会看不见更新。 */
  it('每次操作返回新对象，不动原来那条', async () => {
    const chain: CodecChain = newChain(packed('hi'))
    const next = await extendChain(chain, 'gzip')

    expect(next).not.toBe(chain)
    expect(chain.steps).toHaveLength(0)
    expect(undoStep(next)).not.toBe(next)
    expect(resetChain(next)).not.toBe(next)
  })
})
