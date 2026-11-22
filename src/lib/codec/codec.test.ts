import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  checkRoundTrip,
  decodeValue,
  decodeWithRenderer,
  detectCodec,
  isImplemented,
  READONLY_CODECS,
  RENDERER_CODECS,
  unimplementedReason,
  type RendererCodec,
} from './index'
import { DETECT_CANDIDATES } from '@/lib/codecs'
import { base64FromBytes } from '@/lib/redisText'
import type { RedisText } from '@/types/ipc'

/* IPC 不在测试范围里：这一层要验的是分派与自检的逻辑，
   压缩类到底解得对不对是 Rust 侧 codec.rs 的单元测试在管。 */
const native = vi.hoisted(() => ({
  codecDecode: vi.fn(),
  codecDetect: vi.fn(),
}))
vi.mock('@/api/native', () => native)

/**
 * 注册表是模块级的单例，而这一份用例要么往里塞假实现、要么要求它是空的。
 *
 * **清空要在每个用例之前做，不能只在之后做。** 三个解析器现在都真的挂在表上了，
 * 只在 afterEach 清的话，"注册表里没有就是未实现"那条就依赖前面跑过别的用例；
 * 单独跑它（`vitest -t`）会撞上真实现，失败得莫名其妙。
 */
function emptyRegistry(): void {
  delete RENDERER_CODECS.msgpack
  delete RENDERER_CODECS.phpSerialize
  delete RENDERER_CODECS.protobuf
  // 这两个只可能是用例自己塞进来的（readonly 的实现挂在 READONLY_CODECS 上），
  // 但塞进来之后必须清掉，否则会漏进下一条用例
  delete RENDERER_CODECS.pickle
  delete RENDERER_CODECS.javaSerialize
}

beforeEach(emptyRegistry)

afterEach(() => {
  emptyRegistry()
  vi.clearAllMocks()
})

function source(...bytes: number[]): RedisText {
  return { text: null, base64: base64FromBytes(Uint8Array.from(bytes)), totalBytes: null }
}

/* ── 四个假编解码器，覆盖自检的四种结局 ─────────────────── */

/** 忠实往返：整块字节进一个 bytes 节点，再原样编回去。 */
const faithful: RendererCodec = {
  decode: (bytes) => ({ t: 'bytes', v: bytes }),
  encode: (node) => (node.t === 'bytes' ? node.v : new Uint8Array()),
}

/** 解码时吞掉最后一个字节 —— 重编出来就短一截。 */
const dropsTail: RendererCodec = {
  decode: (bytes) => ({ t: 'bytes', v: bytes.slice(0, bytes.length - 1) }),
  encode: (node) => (node.t === 'bytes' ? node.v : new Uint8Array()),
}

/** 长度对得上但第三个字节不同 —— 只有逐字节比对才抓得住。 */
const corruptsOneByte: RendererCodec = {
  decode: (bytes) => ({ t: 'bytes', v: bytes }),
  encode: (node) => {
    const out = node.t === 'bytes' ? Uint8Array.from(node.v) : new Uint8Array()
    out[2] ^= 0xff
    return out
  },
}

/** 根本编不回去，像 PHP 的 `C:`（内容是类自己写的原始字节，格式不自描述）。 */
const cannotEncode: RendererCodec = {
  decode: (bytes) => ({ t: 'bytes', v: bytes }),
  encode: () => {
    throw new Error('格式不自描述')
  },
}

describe('往返自检', () => {
  const bytes = Uint8Array.from([1, 2, 3, 4])

  it('逐字节相同就允许编辑', () => {
    expect(checkRoundTrip(faithful, faithful.decode(bytes), bytes)).toEqual({ ok: true })
  })

  it('长度不同时转只读，并说出差在哪', () => {
    const editable = checkRoundTrip(dropsTail, dropsTail.decode(bytes), bytes)
    expect(editable.ok).toBe(false)
    expect(editable.ok === false && editable.reason).toContain('长度对不上')
  })

  // 这一条是"不列类型清单"的理由本身：长度一样、类型一样，只有内容变了
  it('长度相同但内容不同时也要抓住，并指出第几个字节', () => {
    const editable = checkRoundTrip(corruptsOneByte, corruptsOneByte.decode(bytes), bytes)
    expect(editable.ok).toBe(false)
    expect(editable.ok === false && editable.reason).toContain('第 3 个字节')
  })

  it('encode 抛错算不可编辑，而不是让整次解码失败', () => {
    const editable = checkRoundTrip(cannotEncode, cannotEncode.decode(bytes), bytes)
    expect(editable.ok).toBe(false)
    expect(editable.ok === false && editable.reason).toContain('格式不自描述')
  })

  it('解码成功就立刻自检，产物带着结论一起出来', () => {
    const decoded = decodeWithRenderer(dropsTail, bytes)
    expect(decoded.kind).toBe('struct')
    expect(decoded.kind === 'struct' && decoded.editable.ok).toBe(false)
  })

  it('decode 抛错原样往上传，不吞成"不可编辑"', () => {
    const broken: RendererCodec = {
      decode: () => {
        throw new Error('不是这个格式')
      },
      encode: () => new Uint8Array(),
    }
    expect(() => decodeWithRenderer(broken, bytes)).toThrow('不是这个格式')
  })
})

describe('能不能跑', () => {
  it('压缩类一直可用', () => {
    expect(isImplemented('gzip')).toBe(true)
    expect(unimplementedReason('gzip')).toBeNull()
  })

  it('序列化类跟着注册表走', () => {
    expect(isImplemented('msgpack')).toBe(false)
    RENDERER_CODECS.msgpack = faithful
    expect(isImplemented('msgpack')).toBe(true)
  })

  /**
   * `readonly` 的实现**挂不进这张注册表**，而且这不是限制、是定义：
   * `RendererCodec` 要求 `encode`（往返自检靠它），而 readonly 的含义
   * 恰恰是"不能编码回去"。它们走 `READONLY_CODECS` 那张单独的表，见
   * doc/frontend/codecs.md「ReadonlyCodec」。
   *
   * 这条用例断言的不变量是"挂错表不算数"。安全边界禁的是**完整支持**（反序列化
   * 即执行），不是禁解析本身；措辞不能写成"永远不做"，否则 `Readonly` 这个
   * conclusion 就没有意义了（doc/backend/codec.md「Readonly 安全边界」）。
   */
  it('readonly 的实现挂进这张注册表不算数', () => {
    // pickle 真的实现了，但它在另一张表上 —— 这里的 emptyRegistry 清不掉它
    expect(isImplemented('pickle')).toBe(true)

    // 两个 readonly 都已实现。临时从正确的表摘掉 Java，再证明塞进错误表不算数。
    const java = READONLY_CODECS.javaSerialize
    delete READONLY_CODECS.javaSerialize
    try {
      expect(isImplemented('javaSerialize')).toBe(false)
      RENDERER_CODECS.javaSerialize = faithful
      expect(isImplemented('javaSerialize')).toBe(false)
      expect(unimplementedReason('javaSerialize')).toContain('只读')
    } finally {
      if (java) READONLY_CODECS.javaSerialize = java
    }
  })
})

describe('分派', () => {
  it('压缩类走 IPC，产物是字节', async () => {
    const value: RedisText = { text: 'hello', base64: null, totalBytes: null }
    native.codecDecode.mockResolvedValue({ value })

    const decoded = await decodeValue('gzip', source(0x1f, 0x8b))

    expect(native.codecDecode).toHaveBeenCalledWith({ codec: 'gzip', inputBase64: 'H4s=' })
    expect(decoded).toEqual({ kind: 'text', value })
  })

  it('序列化类查注册表，一次 IPC 都不发', async () => {
    RENDERER_CODECS.msgpack = faithful

    const decoded = await decodeValue('msgpack', source(1, 2, 3))

    expect(native.codecDecode).not.toHaveBeenCalled()
    expect(decoded.kind).toBe('struct')
    expect(decoded.kind === 'struct' && decoded.root).toEqual({
      t: 'bytes',
      v: Uint8Array.from([1, 2, 3]),
    })
  })

  it('注册表里没有就是未实现，不要退回去问后端', async () => {
    await expect(decodeValue('protobuf', source(1))).rejects.toThrow('还没有实现')
    expect(native.codecDecode).not.toHaveBeenCalled()
  })
})

describe('自动识别', () => {
  /** 只认 `a` 开头，像 PHP 序列化的数组前缀。 */
  const prefixed: RendererCodec = {
    decode: (bytes) => {
      if (bytes[0] !== 0x61) throw new Error('前缀不对')
      return { t: 'arr', v: [] }
    },
    encode: () => Uint8Array.from([0x61]),
  }

  it('本地先试注册表里的，命中就不问后端', async () => {
    RENDERER_CODECS.phpSerialize = prefixed
    native.codecDetect.mockResolvedValue({ codec: null })

    await expect(detectCodec(source(0x61, 0x3a))).resolves.toBe('phpSerialize')
    expect(native.codecDetect).not.toHaveBeenCalled()
  })

  it('本地没命中就把候选集原样交给后端', async () => {
    RENDERER_CODECS.phpSerialize = prefixed
    native.codecDetect.mockResolvedValue({ codec: 'gzip' })

    await expect(detectCodec(source(0x1f, 0x8b))).resolves.toBe('gzip')
    expect(native.codecDetect).toHaveBeenCalledWith({
      inputBase64: 'H4s=',
      candidates: DETECT_CANDIDATES,
    })
  })

  /* doc/frontend/codecs.md「识别的三个维度」的要害：任意字节都可能是合法 msgpack。
     要是候选集跟着"能不能跑"走，
     实现 msgpack 的那一刻起，每一个值都会被识别成 msgpack。 */
  it('实现 msgpack 不会让它混进候选集', async () => {
    RENDERER_CODECS.msgpack = faithful // 这个假实现来者不拒
    native.codecDetect.mockResolvedValue({ codec: null })

    await expect(detectCodec(source(0xde, 0xad))).resolves.toBeNull()
  })
})
