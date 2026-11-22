import { encode as libEncode, decode as libDecode, ExtData } from '@msgpack/msgpack'
import { describe, expect, it, vi } from 'vitest'
import { DETECT_CANDIDATES } from '@/lib/codecs'
import { base64FromBytes } from '@/lib/redisText'
import type { RedisText } from '@/types/ipc'
import { decodeValue, decodeWithRenderer, isImplemented } from './index'
import { msgpackCodec } from './msgpack'
import type { Node } from './types'

/* `./index` 静态 import 了 `@/api/native`，那条链一路通到 @tauri-apps/api。
   msgpack 这一侧一次 IPC 都不该发，所以整个模块换成假的，顺带钉住"没发过"。 */
vi.mock('@/api/native', () => ({
  codecDecode: vi.fn(),
  codecEncode: vi.fn(),
  codecDetect: vi.fn(),
}))

const encoder = new TextEncoder()

/** 字节直写。msgpack 是二进制格式，用例基本都得逐字节拼。 */
function bin(...parts: Array<string | number[]>): Uint8Array {
  const chunks = parts.map((part) =>
    typeof part === 'string' ? encoder.encode(part) : Uint8Array.from(part),
  )
  const out = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.length, 0))
  let at = 0
  for (const chunk of chunks) {
    out.set(chunk, at)
    at += chunk.length
  }
  return out
}

/** 解出来 + 重编回去必须一字节不差，返回解出来的树供进一步断言。 */
function roundTrips(...parts: Array<string | number[]>): Node {
  const input = bin(...parts)
  const node = msgpackCodec.decode(input)
  expect(msgpackCodec.encode(node)).toEqual(input)
  return node
}

/** 走片 1 的自检，拿到的是"能不能编辑"的结论而不只是解析结果。 */
function verdict(...parts: Array<string | number[]>): { ok: boolean; reason: string } {
  const decoded = decodeWithRenderer(msgpackCodec, bin(...parts))
  if (decoded.kind !== 'struct') throw new Error('渲染进程的编解码器只会给出结构')
  return { ok: decoded.editable.ok, reason: decoded.editable.ok ? '' : decoded.editable.reason }
}

function fails(...parts: Array<string | number[]>): void {
  expect(() => msgpackCodec.decode(bin(...parts))).toThrow()
}

/** 2^64，比 uint64 的最大值大一。 */
const UINT64_OVERFLOW = 18446744073709551616n

/* ── 保真：`Node` 的每一格都是为一条真实损失设计的 ────── */

describe('int 一律是 bigint', () => {
  it('每一档整数编码都能往返', () => {
    expect(roundTrips([0x00])).toEqual({ t: 'int', v: 0n }) // positive fixint
    expect(roundTrips([0x7f])).toEqual({ t: 'int', v: 127n })
    expect(roundTrips([0xff])).toEqual({ t: 'int', v: -1n }) // negative fixint
    expect(roundTrips([0xe0])).toEqual({ t: 'int', v: -32n })
    expect(roundTrips([0xcc, 0x80])).toEqual({ t: 'int', v: 128n }) // uint8
    expect(roundTrips([0xcd, 0x01, 0x00])).toEqual({ t: 'int', v: 256n }) // uint16
    expect(roundTrips([0xce, 0x00, 0x01, 0x00, 0x00])).toEqual({ t: 'int', v: 65536n }) // uint32
    expect(roundTrips([0xd0, 0x80])).toEqual({ t: 'int', v: -128n }) // int8
    expect(roundTrips([0xd1, 0x80, 0x00])).toEqual({ t: 'int', v: -32768n }) // int16
    expect(roundTrips([0xd2, 0x80, 0x00, 0x00, 0x00])).toEqual({ t: 'int', v: -2147483648n })
  })

  /**
   * **这一条就是 `Node.int` 用 bigint 的全部理由。** 2^53+1 塞进 number 会静默
   * 变成 2^53，重编出来的字节就是另一个值 —— 而且没有任何地方会报错。
   */
  it('超出 MAX_SAFE_INTEGER 的 int64 一位精度都不丢', () => {
    expect(Number.MAX_SAFE_INTEGER).toBe(9007199254740991)

    // uint64 2^53+1 = 9007199254740993
    expect(roundTrips([0xcf, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01])).toEqual({
      t: 'int',
      v: 9007199254740993n,
    })
    // int64 -(2^53+1)
    expect(roundTrips([0xd3, 0xff, 0xdf, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])).toEqual({
      t: 'int',
      v: -9007199254740993n,
    })
    // int64 最小值
    expect(roundTrips([0xd3, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])).toEqual({
      t: 'int',
      v: -9223372036854775808n,
    })
    // uint64 最大值 —— 它超出 int64 的正端，所以 `Node.int` 的范围是两族的并集
    expect(roundTrips([0xcf, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])).toEqual({
      t: 'int',
      v: 18446744073709551615n,
    })
  })

  it('编不出范围以外的整数', () => {
    expect(() => msgpackCodec.encode({ t: 'int', v: UINT64_OVERFLOW })).toThrow('超出')
    expect(() => msgpackCodec.encode({ t: 'int', v: -9223372036854775809n })).toThrow('超出')
  })
})

describe('str 与 bin 是两族', () => {
  it('同样的字节，str 与 bin 解出两种节点', () => {
    expect(roundTrips([0xa3], 'abc')).toEqual({ t: 'str', v: 'abc' })
    expect(roundTrips([0xc4, 0x03], 'abc')).toEqual({ t: 'bytes', v: encoder.encode('abc') })
  })

  it('每一档长度前缀都能往返', () => {
    const long = 'x'.repeat(40)
    expect(roundTrips([0xd9, 40], long)).toEqual({ t: 'str', v: long }) // str8
    const longer = 'y'.repeat(300)
    expect(roundTrips([0xda, 0x01, 0x2c], longer)).toEqual({ t: 'str', v: longer }) // str16
    expect(roundTrips([0xc5, 0x01, 0x2c], 'z'.repeat(300)).t).toBe('bytes') // bin16
  })

  /**
   * spec 规定 str 是 UTF-8，但实践中有库往里塞任意字节。塞一串 U+FFFD 进去是
   * **篡改数据且不可逆**，所以那时进 `bytes`。
   *
   * 代价要一起钉住：`Node` 分不出"本来是 str"与"本来是 bin"，重编写成 bin，
   * 字节对不上 → 只读。这正是 `checkRoundTrip` 点名的"msgpack 的 str/bin 歧义"。
   */
  it('非 UTF-8 的 str 进 bytes，而且因此判只读', () => {
    const node = msgpackCodec.decode(bin([0xa2, 0xff, 0xfe]))
    expect(node).toEqual({ t: 'bytes', v: Uint8Array.from([0xff, 0xfe]) })
    expect(JSON.stringify(node)).not.toContain('�')

    const result = verdict([0xa2, 0xff, 0xfe])
    expect(result.ok).toBe(false)
    expect(result.reason).toContain('字节')
  })

  /* TextDecoder 默认会**吃掉**开头的 U+FEFF，解出来少三个字节还一声不吭 */
  it('开头的 BOM 是数据，不是标记', () => {
    expect(roundTrips([0xa3, 0xef, 0xbb, 0xbf])).toEqual({ t: 'str', v: '﻿' })
  })

  it('多字节字符的长度数的是字节', () => {
    expect(roundTrips([0xa6], 'héllo')).toEqual({ t: 'str', v: 'héllo' })
    expect(roundTrips([0xa0])).toEqual({ t: 'str', v: '' })
    expect(roundTrips([0xc4, 0x00])).toEqual({ t: 'bytes', v: new Uint8Array(0) })
  })

  /** 解出来的 bytes 不能是输入的视图，否则一个两字节的 bin 会钉住整段 8 MB。 */
  it('bytes 是副本，不吊着输入的引用', () => {
    const input = bin([0xc4, 0x02, 0xaa, 0xbb])
    const node = msgpackCodec.decode(input)
    if (node.t !== 'bytes') throw new Error('应该是 bytes')
    expect(node.v.buffer).not.toBe(input.buffer)
  })
})

describe('float32 与 float64 是两种编码', () => {
  it('宽度如实填进 bits，两种都能往返', () => {
    expect(roundTrips([0xca, 0x3f, 0x80, 0x00, 0x00])).toEqual({ t: 'float', v: 1, bits: 32 })
    expect(roundTrips([0xcb, 0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])).toEqual({
      t: 'float',
      v: 1,
      bits: 64,
    })
    expect(roundTrips([0xcb, 0x3f, 0xb9, 0x99, 0x99, 0x99, 0x99, 0x99, 0x9a])).toEqual({
      t: 'float',
      v: 0.1,
      bits: 64,
    })
  })

  /**
   * **不填 `literal`。** doc/frontend/codec-formats.md「float.literal」说清了：那个字段是给文本格式的
   * （同一个值多种写法），msgpack 的 float 是定宽二进制，表示唯一。
   */
  it('二进制 float 不带 literal', () => {
    for (const input of [
      [0xca, 0x42, 0xc8, 0x00, 0x00],
      [0xcb, 0x40, 0x59, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    ]) {
      const node = msgpackCodec.decode(bin(input))
      expect(node).not.toHaveProperty('literal')
      expect(node.t === 'float' && node.v).toBe(100)
    }
  })

  it('-0 与 0 是两串不同的字节', () => {
    const node = roundTrips([0xcb, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
    expect(node.t === 'float' && Object.is(node.v, -0)).toBe(true)
  })

  it('NaN 与 Infinity 都能往返', () => {
    const nan = roundTrips([0xcb, 0x7f, 0xf8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
    expect(nan.t === 'float' && Number.isNaN(nan.v)).toBe(true)
    expect(roundTrips([0xca, 0x7f, 0x80, 0x00, 0x00])).toEqual({
      t: 'float',
      v: Number.POSITIVE_INFINITY,
      bits: 32,
    })
  })

  /**
   * 只会打到用户**改过**的值：`setFloat32(0.1)` 会悄悄舍成 0.10000000149…，
   * 那是静默改值。解出来的 f32 天然满足 `Math.fround(v) === v`，不受影响。
   */
  it('存不进 float32 的值宁可抛错也不静默舍入', () => {
    expect(() => msgpackCodec.encode({ t: 'float', v: 0.1, bits: 32 })).toThrow('float32')
    expect(msgpackCodec.encode({ t: 'float', v: 0.5, bits: 32 })).toEqual(
      bin([0xca, 0x3f, 0x00, 0x00, 0x00]),
    )
  })
})

describe('map 的键可以不是字符串，而且有序', () => {
  /**
   * 用 `Array<[Node, Node]>` 而不是 `Record` 的全部理由都在这一条里。
   * 库默认解成普通 JS 对象：int 键 `1` 会变成字符串 `"1"`，而且被 JS 的属性
   * 顺序规则提到最前面 —— 类型与顺序同时丢掉。
   */
  it('键的类型与顺序都留住', () => {
    expect(roundTrips([0x83, 0xa1], 'b', [0x01, 0x01, 0xa1], 'x', [0xa1], 'a', [0x02])).toEqual({
      t: 'map',
      v: [
        [{ t: 'str', v: 'b' }, { t: 'int', v: 1n }],
        [{ t: 'int', v: 1n }, { t: 'str', v: 'x' }],
        [{ t: 'str', v: 'a' }, { t: 'int', v: 2n }],
      ],
    })
  })

  it('容器、二进制、甚至 null 都能当键', () => {
    expect(roundTrips([0x82, 0x91, 0x01, 0xa1], 'a', [0xc0, 0xc4, 0x01, 0xff])).toEqual({
      t: 'map',
      v: [
        [{ t: 'arr', v: [{ t: 'int', v: 1n }] }, { t: 'str', v: 'a' }],
        [{ t: 'null' }, { t: 'bytes', v: Uint8Array.from([0xff]) }],
      ],
    })
  })

  /* 重复键是输入的事实。归一掉就再也编不回原字节，而 Map / Record 都会归一 */
  it('重复的键不合并', () => {
    const node = roundTrips([0x82, 0xa1], 'a', [0x01, 0xa1], 'a', [0x02])
    expect(node.t === 'map' && node.v).toHaveLength(2)
  })

  /** 库会拿 `DecodeError` 拒掉这个键，可它是完全合法的 msgpack。 */
  it('__proto__ 当键是合法的 msgpack', () => {
    const node = roundTrips([0x81, 0xa9], '__proto__', [0x01])
    expect(node).toEqual({
      t: 'map',
      v: [[{ t: 'str', v: '__proto__' }, { t: 'int', v: 1n }]],
    })
  })

  it('空容器与 16 位长度前缀', () => {
    expect(roundTrips([0x80])).toEqual({ t: 'map', v: [] })
    expect(roundTrips([0x90])).toEqual({ t: 'arr', v: [] })

    const many = [0xdc, 0x00, 0x14, ...Array.from({ length: 20 }, (_, i) => i)]
    const node = roundTrips(many)
    expect(node.t === 'arr' && node.v).toHaveLength(20)
  })
})

describe('ext', () => {
  it('五档 fixext 与 ext8 都原样收着', () => {
    expect(roundTrips([0xd4, 0x07, 0xaa])).toEqual({
      t: 'ext',
      code: 7,
      v: Uint8Array.from([0xaa]),
    })
    expect(roundTrips([0xd8, 0x01, ...Array(16).fill(0xbb)]).t).toBe('ext')
    expect(roundTrips([0xc7, 0x03, 0x05, 0x01, 0x02, 0x03])).toEqual({
      t: 'ext',
      code: 5,
      v: Uint8Array.from([0x01, 0x02, 0x03]),
    })
    // 长度为 0 的 ext 没有 fix 档，只能走 ext8
    expect(roundTrips([0xc7, 0x00, 0x09])).toEqual({ t: 'ext', code: 9, v: new Uint8Array(0) })
  })

  /** type 是**有符号**的一字节，负号区留给 spec 自己（timestamp 是 -1）。 */
  it('type 是有符号字节', () => {
    expect(roundTrips([0xd4, 0xff, 0x01])).toEqual({
      t: 'ext',
      code: -1,
      v: Uint8Array.from([0x01]),
    })
    expect(() => msgpackCodec.encode({ t: 'ext', code: 128, v: new Uint8Array(1) })).toThrow()
  })

  /**
   * timestamp 只是 ext 的一个取值（type = -1），所以**不特判**：原样进
   * `Node.ext`，`types.ts` 的 `Node` 就是这么规定的。库会把它解成 `Date` —— 那一步会把
   * 32 / 64 / 96 三种编码抹平成同一个 JS 值，纳秒也留不住。
   */
  it('timestamp 不解释成日期，三种宽度都原样往返', () => {
    expect(roundTrips([0xd6, 0xff, 0x00, 0x00, 0x00, 0x00])).toEqual({
      t: 'ext',
      code: -1,
      v: Uint8Array.from([0, 0, 0, 0]),
    })
    expect(roundTrips([0xd7, 0xff, ...Array(8).fill(0x00)]).t).toBe('ext') // timestamp 64
    expect(roundTrips([0xc7, 0x0c, 0xff, ...Array(12).fill(0x00)]).t).toBe('ext') // timestamp 96
  })
})

describe('嵌套', () => {
  it('map 里套 array 里套 map', () => {
    expect(
      roundTrips([0x81, 0xa1], 'a', [0x92, 0x81, 0xa1], 'b', [0xc3, 0xc0]),
    ).toEqual({
      t: 'map',
      v: [
        [
          { t: 'str', v: 'a' },
          {
            t: 'arr',
            v: [
              { t: 'map', v: [[{ t: 'str', v: 'b' }, { t: 'bool', v: true }]] },
              { t: 'null' },
            ],
          },
        ],
      ],
    })
  })

  /** 递归下降会炸栈，而炸栈抛的是 RangeError，混在"解不开"里看不出出了什么事。 */
  it('嵌套过深要抛错而不是炸栈', () => {
    expect(() => msgpackCodec.decode(bin(Array(2000).fill(0x91).concat([0x01])))).toThrow('嵌套')
  })
})

describe('标量', () => {
  it('nil 与 bool', () => {
    expect(roundTrips([0xc0])).toEqual({ t: 'null' })
    expect(roundTrips([0xc2])).toEqual({ t: 'bool', v: false })
    expect(roundTrips([0xc3])).toEqual({ t: 'bool', v: true })
  })
})

/* ── 严格：解完必须一个字节都不剩 ─────────────────────── */

describe('严格性', () => {
  it('尾部有多余字节要抛错', () => {
    fails([0x01, 0x02])
    fails([0xc0, 0xc0])
    fails([0x91, 0x01], 'x')
  })

  it('截断的输入要抛错', () => {
    fails([])
    fails([0xcc]) // uint8 少了值
    fails([0xcf, 0x00]) // uint64 只给了一个字节
    fails([0xa5], 'abc') // fixstr 说 5 个字节只给了 3
    fails([0x92, 0x01]) // fixarray 说 2 项只给了 1
    fails([0x81, 0xa1], 'a') // fixmap 的值没了
    fails([0xd4]) // fixext 少了 type
  })

  it('spec 里标着 never used 的 0xc1 要拒掉', () => {
    fails([0xc1])
  })

  /** `dd ff ff ff ff` 声明四十亿项。没有守卫就会先去转四十亿次循环。 */
  it('声明的项数超出剩余字节要立刻抛错', () => {
    fails([0xdd, 0xff, 0xff, 0xff, 0xff])
    fails([0xdf, 0xff, 0xff, 0xff, 0xff])
    fails([0xdb, 0xff, 0xff, 0xff, 0xff])
  })

  it('错误信息说得出位置', () => {
    expect(() => msgpackCodec.decode(bin([0x91, 0x01, 0x02]))).toThrow('偏移 2')
  })
})

/* ── 不可往返的那几种，判定要走片 1 的自检 ────────────── */

describe('往返自检的结论', () => {
  it('正常的值可以编辑', () => {
    expect(verdict([0x82, 0xa1], 'a', [0x01, 0xa1], 'b', [0xc3]).ok).toBe(true)
  })

  /**
   * **这一条是这一片最该钉住的行为。** msgpack 允许同一个值用不同宽度编码：
   * `1` 可以是 fixint `0x01`，也可以是 uint8 `0xcc 0x01`。主流库都写最短形式，
   * 所以非最短编码的输入重编后字节对不上 → 自检判只读。
   *
   * **这是正确行为，不要为它开特例。** 跟 PHP 的 float 字面量不同：那里是标准
   * PHP 自己就产出多种写法，属于常态；msgpack 的非最短编码是罕见的非标准输出。
   */
  it('非最短编码解得开，但判成只读', () => {
    // 值本身解得对
    expect(msgpackCodec.decode(bin([0xcc, 0x01]))).toEqual({ t: 'int', v: 1n })
    expect(msgpackCodec.decode(bin([0xd9, 0x01], 'a'))).toEqual({ t: 'str', v: 'a' })

    // 但重编成最短形式，字节对不上
    expect(msgpackCodec.encode({ t: 'int', v: 1n })).toEqual(bin([0x01]))
    expect(verdict([0xcc, 0x01]).ok).toBe(false)
    expect(verdict([0xd9, 0x01], 'a').ok).toBe(false) // str8 装得下 fixstr 的内容
    expect(verdict([0xdc, 0x00, 0x01, 0x01]).ok).toBe(false) // array16 装一项
    expect(verdict([0xc8, 0x00, 0x01, 0x07, 0xaa]).ok).toBe(false) // ext16 装一字节
  })

  it('只读理由说得出是哪里对不上', () => {
    const result = verdict([0xcc, 0x01])
    expect(result.ok).toBe(false)
    expect(result.reason).toMatch(/长度|字节/)
  })

  /** 解出来是 obj / field 的树不该出现在这条路径上，但编辑路径可以造出来。 */
  it('PHP 的类与 protobuf 的字段编不进 msgpack', () => {
    expect(() => msgpackCodec.encode({ t: 'obj', cls: 'Foo', v: [] })).toThrow()
    expect(() =>
      msgpackCodec.encode({ t: 'field', no: 1, wire: 2, v: { t: 'null' } }),
    ).toThrow()
  })
})

/* ── 对照：拿 @msgpack/msgpack 当独立实现交叉验证 ─────── */

/**
 * `@msgpack/msgpack` 是 **devDependency**，只在这里出现。
 *
 * 它不能当运行时依赖（详见 doc/frontend/codec-formats.md「不用 @msgpack/msgpack 的
 * 五条理由」：map 只能是普通对象、`useBigInt64` 让每个小整数都写满 8 字节、float
 * 宽度是全局开关、非 UTF-8 的 str 拿不回原字节），因为它的值模型是**惯用 JS 值**，
 * 而 `Node` 存在的理由正是那装不下 msgpack。
 *
 * 但在**它能表达的那个子集**上，它仍然是一份被大量 fuzz 过的独立实现 ——
 * 那正是"格式细节的正确性"能买到的地方，所以放在测试里当对照：
 *
 *   - 我们写的字节，它得读成同一个值（验编码器的头字节、字节序、长度前缀）
 *   - 它写的字节，我们得**逐字节**往返（验"最短形式"这个前提确实是主流写法，
 *     也就是上面那条"非最短判只读"不会误伤正常数据）
 */
describe('与 @msgpack/msgpack 交叉验证', () => {
  const samples: Array<[string, unknown]> = [
    ['nil', null],
    ['bool', true],
    ['small int', 1],
    ['uint8 边界', 200],
    ['uint16 边界', 40000],
    ['uint32 边界', 70000],
    ['negative fixint', -5],
    ['int32', -70000],
    ['float64', 0.1],
    ['str', 'héllo'],
    ['空 str', ''],
    ['长 str', 'x'.repeat(300)],
    ['bin', Uint8Array.from([0, 1, 255])],
    ['array', [1, 'a', null, true]],
    ['map', { a: 1, b: [2, 3] }],
    ['嵌套', { list: [{ k: 'v' }, [1, 2]], n: null }],
    ['ext', new ExtData(7, Uint8Array.from([1, 2, 3]))],
    ['timestamp', new Date(0)],
  ]

  it.each(samples)('它写的 %s，我们逐字节往返', (_label, value) => {
    const bytes = libEncode(value)
    expect(msgpackCodec.encode(msgpackCodec.decode(bytes))).toEqual(bytes)
    // 主流写库只写最短形式 → 正常数据一定可编辑，"非最短判只读"不误伤
    const decoded = decodeWithRenderer(msgpackCodec, bytes)
    expect(decoded.kind === 'struct' && decoded.editable).toEqual({ ok: true })
  })

  it.each(samples)('我们写的 %s，它读回同一个值', (_label, value) => {
    const node = msgpackCodec.decode(libEncode(value))
    expect(libDecode(msgpackCodec.encode(node))).toEqual(libDecode(libEncode(value)))
  })

  /** 大整数要开 `useBigInt64` 才读得回来，顺带钉住我们写的是哪一族。 */
  it('int64 / uint64 的字节它也认', () => {
    for (const value of [9007199254740993n, -9007199254740993n, 18446744073709551615n]) {
      const bytes = msgpackCodec.encode({ t: 'int', v: value })
      expect(libDecode(bytes, { useBigInt64: true })).toBe(value)
    }
  })

  /** float32 那一档 `libEncode` 写不出来（`forceFloat32` 是全局开关），单独验。 */
  it('我们写的 float32 它读得对', () => {
    const bytes = msgpackCodec.encode({ t: 'float', v: 0.5, bits: 32 })
    expect(bytes).toEqual(bin([0xca, 0x3f, 0x00, 0x00, 0x00]))
    expect(libDecode(bytes)).toBe(0.5)
  })
})

/* ── 挂上去了没有 ─────────────────────────────────────── */

describe('注册', () => {
  it('msgpack 现在能跑了', () => {
    expect(isImplemented('msgpack')).toBe(true)
  })

  /**
   * **能不能跑与能不能自动识别是两个维度。** 任意字节都可能是合法 msgpack，
   * 进了候选就会把一切都识别成它 —— 所以 `detectable` 实现了也不能跟着变。
   * 见 doc/frontend/codecs.md「识别的三个维度」。
   */
  it('实现了也不能进自动识别的候选集', () => {
    expect(DETECT_CANDIDATES).not.toContain('msgpack')
  })

  it('走 decodeValue 分派下来，一次 IPC 都不发', async () => {
    const value: RedisText = {
      text: null,
      base64: base64FromBytes(bin([0x81, 0xa1], 'a', [0xc3])),
      totalBytes: null,
    }
    const decoded = await decodeValue('msgpack', value)

    expect(decoded.kind).toBe('struct')
    expect(decoded.kind === 'struct' && decoded.editable).toEqual({ ok: true })
    expect(decoded.kind === 'struct' && decoded.root).toEqual({
      t: 'map',
      v: [[{ t: 'str', v: 'a' }, { t: 'bool', v: true }]],
    })
  })
})
