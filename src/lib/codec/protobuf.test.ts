import { describe, expect, it, vi } from 'vitest'
import { DETECT_CANDIDATES } from '@/lib/codecs'
import { base64FromBytes } from '@/lib/redisText'
import type { RedisText } from '@/types/ipc'
import { decodeValue, decodeWithRenderer, isImplemented } from './index'
import { protobufCodec } from './protobuf'
import { renderNode } from './render'
import type { Node } from './types'

/* `./index` 静态 import 了 `@/api/native`，那条链一路通到 @tauri-apps/api。
   protobuf 这一侧一次 IPC 都不该发，所以整个模块换成假的，顺带钉住"没发过"。 */
vi.mock('@/api/native', () => ({
  codecDecode: vi.fn(),
  codecEncode: vi.fn(),
  codecDetect: vi.fn(),
}))

const encoder = new TextEncoder()

/** 字节直写。protobuf 是二进制格式，用例基本都得逐字节拼。 */
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
  const node = protobufCodec.decode(input)
  expect(protobufCodec.encode(node)).toEqual(input)
  return node
}

/** 走片 1 的自检，拿到的是"能不能编辑"的结论而不只是解析结果。 */
function verdict(...parts: Array<string | number[]>): { ok: boolean; reason: string } {
  const decoded = decodeWithRenderer(protobufCodec, bin(...parts))
  if (decoded.kind !== 'struct') throw new Error('渲染进程的编解码器只会给出结构')
  return { ok: decoded.editable.ok, reason: decoded.editable.ok ? '' : decoded.editable.reason }
}

function fails(...parts: Array<string | number[]>): void {
  expect(() => protobufCodec.decode(bin(...parts))).toThrow()
}

/** 解出来的树只有一个字段时，把那个字段掏出来。 */
function onlyField(node: Node): Node {
  if (node.t !== 'arr' || node.v.length !== 1) throw new Error('这里应该正好是一个字段')
  return node.v[0]
}

/** `tag = (field_number << 3) | wire_type`，用例里到处要拼。 */
function tag(no: number, wire: number): number {
  return (no << 3) | wire
}

/** 长度前缀 + 内容。深嵌套的用例会造出 128 字节以上的 payload，不能只写一个字节。 */
function lenPrefixed(payload: Uint8Array): Uint8Array {
  const prefix: number[] = []
  let len = payload.length
  while (len >= 0x80) {
    prefix.push((len & 0x7f) | 0x80)
    len >>>= 7
  }
  prefix.push(len)
  return bin(prefix, [...payload])
}

/* ── 四种 wire type ───────────────────────────────────── */

describe('wire type', () => {
  /** Encoding 文档的头一个例子：`Test1 { a: 150 }` 就是 `08 96 01`。 */
  it('varint 往返，值是原始无符号数', () => {
    expect(roundTrips([tag(1, 0), 0x96, 0x01])).toEqual({
      t: 'arr',
      v: [{ t: 'field', no: 1, wire: 0, v: { t: 'int', v: 150n } }],
    })
  })

  it('多字节 varint 的每一档都能往返', () => {
    expect(onlyField(roundTrips([tag(1, 0), 0x7f]))).toMatchObject({ v: { v: 127n } })
    expect(onlyField(roundTrips([tag(1, 0), 0x80, 0x01]))).toMatchObject({ v: { v: 128n } })
    expect(onlyField(roundTrips([tag(1, 0), 0xac, 0x02]))).toMatchObject({ v: { v: 300n } })
    // uint64 的最大值：7 位一组，64 位要 10 组
    expect(
      onlyField(roundTrips([tag(1, 0), 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01])),
    ).toMatchObject({ v: { v: 18446744073709551615n } })
  })

  /**
   * **定宽的那两种收成整数，不是 float。** wire 1 是 `fixed64` / `sfixed64` /
   * `double` 共用的，wire 5 是 `fixed32` / `sfixed32` / `float` 共用的，无 schema
   * 时分不出来 —— 收成原始位是唯一无损的形态，理由见 `protobuf.ts` 文件头。
   */
  it('fixed32 / fixed64 往返，收的是原始位而不是浮点', () => {
    // 00 00 80 3f 小端 = 0x3f800000，也就是 float 1.0
    expect(onlyField(roundTrips([tag(1, 5), 0x00, 0x00, 0x80, 0x3f]))).toEqual({
      t: 'field',
      no: 1,
      wire: 5,
      v: { t: 'int', v: 1065353216n },
    })
    // 小端的 double 1.0
    expect(
      onlyField(roundTrips([tag(2, 1), 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x3f])),
    ).toEqual({
      t: 'field',
      no: 2,
      wire: 1,
      v: { t: 'int', v: 4607182418800017408n },
    })
  })

  /**
   * **这一条是不填 `float` 的实证。** 小的负 `sfixed32` / `sfixed64` 的字节全都
   * 落在 IEEE 754 的 NaN 位型上（指数全 1、尾数非零），而 ECMAScript 对
   * `DataView.setFloat32/64` 写 NaN 只承诺"某个实现定义的位型"。收成 float 节点，
   * 在会 purify NaN 的引擎上（JSC 的 NaN-boxing）这些值就会被自检误判成只读。
   */
  it('负的 sfixed 落在 NaN 位型上，收成整数才不会被引擎抹掉', () => {
    for (const bytes of [0xff, 0xfe, 0x9c]) {
      const raw = new DataView(new ArrayBuffer(4))
      raw.setUint8(0, bytes)
      raw.setUint8(1, 0xff)
      raw.setUint8(2, 0xff)
      raw.setUint8(3, 0xff)
      expect(Number.isNaN(raw.getFloat32(0, true))).toBe(true)
    }
    // sfixed32 = -1 与 sfixed64 = -1，两条都必须一字节不差地往返
    expect(onlyField(roundTrips([tag(1, 5), 0xff, 0xff, 0xff, 0xff]))).toMatchObject({
      v: { v: 4294967295n },
    })
    expect(verdict([tag(1, 5), 0xff, 0xff, 0xff, 0xff]).ok).toBe(true)
    expect(verdict([tag(1, 1), ...Array(8).fill(0xff)]).ok).toBe(true)
  })

  /** `Test2 { b: "testing" }` = `12 07 74 65 73 74 69 6e 67`。 */
  it('length-delimited 往返', () => {
    expect(roundTrips([tag(2, 2), 0x07], 'testing')).toEqual({
      t: 'arr',
      v: [{ t: 'field', no: 2, wire: 2, v: { t: 'str', v: 'testing' } }],
    })
  })

  it('空 message 与空 payload 都是合法编码', () => {
    expect(protobufCodec.decode(new Uint8Array())).toEqual({ t: 'arr', v: [] })
    expect(protobufCodec.encode({ t: 'arr', v: [] })).toEqual(new Uint8Array())
    // 空 payload 三种猜法都成立；选空串是因为空 message 会平白多一层嵌套
    expect(onlyField(roundTrips([tag(1, 2), 0x00]))).toEqual({
      t: 'field',
      no: 1,
      wire: 2,
      v: { t: 'str', v: '' },
    })
  })

  it('字段号顶到 2^29-1 也能往返，再大就抛错', () => {
    const node = roundTrips([tag(1, 0), 0x01, 0xf8, 0xff, 0xff, 0xff, 0x0f, 0x2a])
    expect(node.t === 'arr' && node.v[1]).toMatchObject({ no: 536870911, wire: 0 })
    // 2^29 << 3 = 2^32，tag 装不下
    fails([0x80, 0x80, 0x80, 0x80, 0x10, 0x01])
  })
})

/* ── 无 schema 的两条歧义，都不猜 ─────────────────────── */

describe('负数无从判断，所以收原始值', () => {
  /**
   * `int64 -1` 在 wire 上是十字节的补码，`sint64 -1` 是 zigzag 之后的 `1`，
   * `uint64 1` 也是 `1` —— **字节一样的东西无 schema 时就是分不出来**。
   * 所以解析器一律收 `[0, 2^64-1]` 的原始无符号值，一个符号都不还原；
   * "它可能是什么"交给 `render.ts` 显示，见下面的展示用例。
   */
  it('int64 -1 与 uint64 最大值是同一串字节', () => {
    const asInt64 = [tag(1, 0), 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]
    expect(onlyField(roundTrips(asInt64))).toMatchObject({ v: { v: 18446744073709551615n } })
  })

  it('sint64 的 zigzag 不还原，1 就是 1', () => {
    expect(onlyField(roundTrips([tag(1, 0), 0x01]))).toMatchObject({ v: { v: 1n } })
  })
})

/* ── repeated、嵌套、group ───────────────────────────── */

describe('结构', () => {
  /**
   * **这一条是 message 用 `arr` 而不是 `map` 的全部理由。** 同一个字段号可以
   * 出现多次（repeated 的非 packed 写法），`map` 表达不了，顺序也会丢，
   * 而顺序是往返要的字节的一部分。
   */
  it('同一个字段号可以出现多次，顺序留住', () => {
    const node = roundTrips([tag(1, 0), 0x01, tag(1, 0), 0x02, tag(2, 0), 0x03])
    expect(node).toEqual({
      t: 'arr',
      v: [
        { t: 'field', no: 1, wire: 0, v: { t: 'int', v: 1n } },
        { t: 'field', no: 1, wire: 0, v: { t: 'int', v: 2n } },
        { t: 'field', no: 2, wire: 0, v: { t: 'int', v: 3n } },
      ],
    })
  })

  /** `Test3 { c: Test1 { a: 150 } }` = `1a 03 08 96 01`。 */
  it('嵌套 message 往返', () => {
    expect(roundTrips([tag(3, 2), 0x03, tag(1, 0), 0x96, 0x01])).toEqual({
      t: 'arr',
      v: [
        {
          t: 'field',
          no: 3,
          wire: 2,
          v: { t: 'arr', v: [{ t: 'field', no: 1, wire: 0, v: { t: 'int', v: 150n } }] },
        },
      ],
    })
  })

  it('group 忠实解析，往返一字节不差', () => {
    expect(roundTrips([tag(1, 3), tag(2, 0), 0x2a, tag(1, 4)])).toEqual({
      t: 'arr',
      v: [
        {
          t: 'field',
          no: 1,
          wire: 3,
          v: { t: 'arr', v: [{ t: 'field', no: 2, wire: 0, v: { t: 'int', v: 42n } }] },
        },
      ],
    })
    // group 里套 group
    expect(roundTrips([tag(1, 3), tag(2, 3), tag(2, 4), tag(1, 4)]).t).toBe('arr')
  })

  /** 不平衡的 group 是**默默出错的口子**，所以三种都得抛。 */
  it('group 不平衡要抛错', () => {
    fails([tag(1, 3), tag(2, 0), 0x2a]) // 到末尾还没闭合
    fails([tag(1, 3), tag(2, 4)]) // 结束标记的字段号对不上
    fails([tag(1, 4)]) // 落单的结束标记
    // group 不能跨越 LEN 记录的边界：payload 里开的 group 只能在 payload 里闭合
    fails([tag(1, 2), 0x01, tag(1, 3), tag(1, 4)])
  })
})

/* ── 呈现是猜测，但猜测不影响往返（doc/frontend/codec-formats.md「Protobuf」） ── */

describe('length-delimited 的三级猜测', () => {
  it('解得开就猜 message，而且照样忠实往返', () => {
    const node = onlyField(roundTrips([tag(1, 2), 0x03, tag(1, 0), 0x96, 0x01]))
    expect(node).toMatchObject({ wire: 2, v: { t: 'arr' } })
    expect(renderNode(node)).toContain('~msg')
  })

  /**
   * 误判是这条规则**内建的代价**：`08 01` 只是两个字节，它既可能是一段
   * 二字节的 bytes 字段，也确实是一个合法的嵌套 message。无 schema 时没有
   * 任何办法分辨 —— 所以标注 `~msg` 而不是直接摆出来，用户看见的是"这是猜的"。
   * 而**保真不受影响**：猜错了字节也一样对得上。
   */
  it('凑巧解得开的字节也会被猜成 message，但字节照样对得上', () => {
    const node = onlyField(roundTrips([tag(1, 2), 0x02, 0x08, 0x01]))
    expect(node).toMatchObject({ v: { t: 'arr' } })
    expect(verdict([tag(1, 2), 0x02, 0x08, 0x01]).ok).toBe(true)
  })

  it('解不开但是文本就猜 string', () => {
    const node = onlyField(roundTrips([tag(2, 2), 0x07], 'testing'))
    expect(node).toMatchObject({ v: { t: 'str', v: 'testing' } })
    expect(renderNode(node)).toContain('~str')
  })

  it('两级都不成立就退回字节', () => {
    // ff fe：当 tag 读是个没读完的 varint，当文本又不是合法 UTF-8
    const node = onlyField(roundTrips([tag(1, 2), 0x02, 0xff, 0xfe]))
    expect(node).toMatchObject({ v: { t: 'bytes', v: Uint8Array.from([0xff, 0xfe]) } })
    expect(renderNode(node)).toContain('~bytes')
  })

  /**
   * packed repeated 与任意字节在 wire 上**没有区别**（都是一条 LEN 记录），
   * 所以这里不去猜"这是三个 varint"。Encoding 文档的例子：
   * 字段 4 packed `[3, 270, 86942]` = `22 06 03 8E 02 9E A7 05`。
   */
  it('packed repeated 如实当字节，往返不受影响', () => {
    const node = onlyField(roundTrips([tag(4, 2), 0x06, 0x03, 0x8e, 0x02, 0x9e, 0xa7, 0x05]))
    expect(node).toMatchObject({ v: { t: 'bytes' } })
    expect(node.t === 'field' && node.v.t === 'bytes' && node.v.v.length).toBe(6)
  })

  /**
   * 合法 UTF-8 还不够：`01 02 03` 是合法 UTF-8，猜成字符串就是三个不可见字符。
   * C0 控制字符（`\t` `\n` `\r` 除外）在真实的 string 字段里几乎不出现，
   * 在 packed repeated 与短二进制里却到处都是。
   */
  it('控制字符退回字节，制表 / 换行仍然算文本', () => {
    expect(onlyField(roundTrips([tag(1, 2), 0x03, 0x01, 0x02, 0x03]))).toMatchObject({
      v: { t: 'bytes' },
    })
    expect(onlyField(roundTrips([tag(1, 2), 0x03], 'a\nb'))).toMatchObject({
      v: { t: 'str', v: 'a\nb' },
    })
  })

  /**
   * **这一条是"猜测不影响往返"的要害。** 一段字节被猜成嵌套 message 之后，
   * 长度前缀必须跟着**内部重编的实际长度**走，而不是记下来的原长度 ——
   * 内部有非最短编码时长度会变，那时整条记录就该对不上并判只读。
   */
  it('猜成 message 时长度前缀跟着内部重编走', () => {
    // payload `08 96 80 00`：内层是非最短编码的 22，重编成 `08 16` 少两个字节
    const input = bin([tag(1, 2), 0x04, tag(1, 0), 0x96, 0x80, 0x00])
    const node = protobufCodec.decode(input)
    expect(onlyField(node)).toMatchObject({ v: { t: 'arr' } })

    expect(protobufCodec.encode(node)).toEqual(bin([tag(1, 2), 0x02, tag(1, 0), 0x16]))
    expect(verdict([tag(1, 2), 0x04, tag(1, 0), 0x96, 0x80, 0x00]).ok).toBe(false)
  })

  /** 解出来的 bytes 不能是输入的视图，否则一个两字节的字段会钉住整段 8 MB。 */
  it('bytes 是副本，不吊着输入的引用', () => {
    const input = bin([tag(1, 2), 0x02, 0xff, 0xfe])
    const node = onlyField(protobufCodec.decode(input))
    if (node.t !== 'field' || node.v.t !== 'bytes') throw new Error('应该是 bytes')
    expect(node.v.v.buffer).not.toBe(input.buffer)
  })
})

/* ── 严格：解不开就抛，不返回"部分解出来"的结构 ────────── */

describe('严格性', () => {
  it('垃圾输入要抛错', () => {
    fails([0x00]) // 字段号 0 非法
    fails([0x07, 0x01]) // 同上（低三位是 wire type 7，但字段号先卡住）
    fails([0xff]) // 没读完的 varint tag
    fails([tag(1, 0)]) // 只有 tag，没有值
    fails([tag(1, 6), 0x01]) // wire type 6 没有定义
    fails([tag(1, 7), 0x01])
    fails('hello world') // 0x68 = #13 varint，接着的 0x6c 是落单的 group 结束标记
  })

  it('长度前缀越界要抛错', () => {
    fails([tag(1, 2), 0x05, 0x01]) // 说有 5 个字节，只剩 1 个
    fails([tag(1, 2), 0xff, 0xff, 0xff, 0xff, 0x0f])
    fails([tag(1, 1), 0x00, 0x00]) // fixed64 只给了两个字节
    fails([tag(1, 5), 0x00])
  })

  /**
   * protobuf 没有"一个值"的边界：根 message 就是一串记录，一路读到末尾。
   * 所以"尾部多余字节"表现为**那段字节自己得是一条合法记录**，
   * 不是的话就在那里抛错 —— 而不是解完之后剩下一截。
   */
  it('尾部多余的字节自己不是合法记录时抛错', () => {
    fails([tag(1, 0), 0x96, 0x01, 0xff]) // 尾巴是个没读完的 varint
    fails([tag(1, 0), 0x96, 0x01, 0x00]) // 尾巴的字段号是 0
    fails([tag(1, 2), 0x01, 0x61, 0x9c]) // payload 之后跟着半个 tag
  })

  it('varint 超过十个字节 / 超出 64 位要抛错', () => {
    fails([tag(1, 0), ...Array(10).fill(0xff), 0x01])
    // 第十个字节把值顶过了 2^64
    fails([tag(1, 0), 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02])
  })

  it('错误信息说得出位置', () => {
    expect(() => protobufCodec.decode(bin([tag(1, 0), 0x01, 0x00]))).toThrow('偏移 2')
  })

  /**
   * 递归下降会炸栈，而炸栈抛的是 `RangeError`，混在"解不开"里看不出出了什么事。
   *
   * 两条路的结局**不一样**，都得钉住：LEN 记录是"试解"，超深了自然试解失败，
   * 于是**降级**成十六进制字节并且照样一字节不差地往返；group 没有长度前缀，
   * 跳不过去，只能抛错。
   */
  it('超深的 LEN 嵌套降级成字节，仍然忠实往返', () => {
    let payload: Uint8Array = new Uint8Array()
    for (let i = 0; i < 300; i += 1) {
      payload = bin([tag(1, 2)], [...lenPrefixed(payload)])
    }
    const node = protobufCodec.decode(payload)
    expect(protobufCodec.encode(node)).toEqual(payload)

    // 一路钻到底：试解在深度上限那里停下，剩下的原样收成字节
    let cursor: Node = node
    let levels = 0
    while (cursor.t === 'arr' && cursor.v.length === 1 && cursor.v[0].t === 'field') {
      cursor = cursor.v[0].v
      levels += 1
    }
    expect(cursor.t).toBe('bytes')
    expect(levels).toBeLessThan(300)
  })

  it('超深的 group 嵌套抛错而不是炸栈', () => {
    fails([...Array(300).fill(tag(1, 3)), ...Array(300).fill(tag(1, 4))])
  })
})

/* ── 往返自检的结论（doc/frontend/codecs.md「往返自检」） ──────── */

describe('往返自检', () => {
  it('正常的值可以编辑', () => {
    expect(verdict([tag(1, 0), 0x96, 0x01, tag(2, 2), 0x07], 'testing').ok).toBe(true)
    expect(verdict([tag(1, 3), tag(2, 0), 0x2a, tag(1, 4)]).ok).toBe(true)
  })

  /**
   * **这一条是这一片最该钉住的行为。** wire format 允许非最短的 varint
   * （`80 00` 也是 0），而真实的 protobuf 库都只写最短形式。所以非最短编码的
   * 输入重编后字节对不上 → 自检判只读。
   *
   * **这是正确行为，不要为它开特例。** 跟 PHP 的 float 字面量不同：那里是标准
   * PHP 自己就产出多种写法，属于常态；非最短 varint 是罕见的非标准输出，
   * 为它留后门等于放弃自检。
   */
  it('非最短的 varint 解得开，但判成只读', () => {
    // 值本身解得对
    expect(onlyField(protobufCodec.decode(bin([tag(1, 0), 0x80, 0x00])))).toMatchObject({
      v: { v: 0n },
    })
    // 但重编成最短形式，字节对不上
    expect(protobufCodec.encode({ t: 'arr', v: [{ t: 'field', no: 1, wire: 0, v: { t: 'int', v: 0n } }] }))
      .toEqual(bin([tag(1, 0), 0x00]))
    expect(verdict([tag(1, 0), 0x80, 0x00]).ok).toBe(false)
    // tag 自己也可以写成非最短形式
    expect(verdict([0x88, 0x00, 0x01]).ok).toBe(false)
    // group 的结束 tag 同理
    expect(verdict([tag(1, 3), 0x8c, 0x00]).ok).toBe(false)
  })

  it('只读理由说得出是哪里对不上', () => {
    const result = verdict([tag(1, 0), 0x80, 0x00])
    expect(result.ok).toBe(false)
    expect(result.reason).toMatch(/长度|字节/)
  })

  /** 编辑路径可以造出解析器不会产出的树，`encode` 得挡住而不是写出坏字节。 */
  it('编不出结构对不上的树', () => {
    const field = (over: Partial<{ no: number; wire: number; v: Node }>): Node => ({
      t: 'field',
      no: 1,
      wire: 0,
      v: { t: 'int', v: 1n },
      ...over,
    })

    expect(() => protobufCodec.encode({ t: 'int', v: 1n })).toThrow('顶层')
    expect(() => protobufCodec.encode({ t: 'arr', v: [{ t: 'int', v: 1n }] })).toThrow('字段')
    expect(() => protobufCodec.encode({ t: 'arr', v: [field({ no: 0 })] })).toThrow('字段号')
    expect(() =>
      protobufCodec.encode({ t: 'arr', v: [field({ v: { t: 'str', v: 'x' } })] }),
    ).toThrow('整数')
    expect(() =>
      protobufCodec.encode({ t: 'arr', v: [field({ wire: 5, v: { t: 'int', v: 4294967296n } })] }),
    ).toThrow('0..')
    // wire 4 只能当 group 的终止符，不能自己当一个字段
    expect(() => protobufCodec.encode({ t: 'arr', v: [field({ wire: 4 })] })).toThrow('wire type 4')
    expect(() =>
      protobufCodec.encode({ t: 'arr', v: [field({ wire: 2, v: { t: 'null' } })] }),
    ).toThrow('wire type 2')
  })
})

/* ── 呈现：把猜测与另一种读法摆给用户看 ───────────────── */

describe('展示', () => {
  it('wire type 用名字，猜测显式标注', () => {
    expect(renderNode(protobufCodec.decode(bin([tag(1, 0), 0x96, 0x01])))).toBe(
      '[\n  #1 varint => i64 150\n]',
    )
    expect(renderNode(protobufCodec.decode(bin([tag(2, 2), 0x07], 'testing')))).toBe(
      '[\n  #2 len => ~str "testing"\n]',
    )
    expect(renderNode(protobufCodec.decode(bin([tag(1, 2), 0x02, 0xff, 0xfe])))).toBe(
      '[\n  #1 len => ~bytes bytes(2) ff fe\n]',
    )
  })

  /**
   * 定宽的那两种收的是原始位，所以浮点读法必须由展示层给出 ——
   * 光看 `4607182418800017408` 没人认得出这是 1.0。
   */
  it('fixed32 / fixed64 同时给出浮点读法', () => {
    expect(renderNode(protobufCodec.decode(bin([tag(1, 5), 0x00, 0x00, 0x80, 0x3f])))).toBe(
      '[\n  #1 fixed32 => i64 1065353216 (float 1)\n]',
    )
    expect(
      renderNode(
        protobufCodec.decode(bin([tag(1, 1), 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x3f])),
      ),
    ).toBe('[\n  #1 fixed64 => i64 4607182418800017408 (double 1)\n]')
  })

  /** raw ≥ 2^63 时补码读法几乎肯定就是真相；zigzag 对每个值都成立，所以不显示。 */
  it('大 varint 给出补码读法，zigzag 不猜', () => {
    const asInt64 = [tag(1, 0), 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]
    expect(renderNode(protobufCodec.decode(bin(asInt64)))).toContain('(int64 -1)')
    expect(renderNode(protobufCodec.decode(bin([tag(1, 0), 0x01])))).toBe(
      '[\n  #1 varint => i64 1\n]',
    )
  })

  it('嵌套 message 与 group 都摆成缩进的块', () => {
    expect(renderNode(protobufCodec.decode(bin([tag(3, 2), 0x03, tag(1, 0), 0x96, 0x01])))).toBe(
      '[\n  #3 len => ~msg [\n    #1 varint => i64 150\n  ]\n]',
    )
    expect(renderNode(protobufCodec.decode(bin([tag(1, 3), tag(2, 0), 0x2a, tag(1, 4)])))).toBe(
      '[\n  #1 group => [\n    #2 varint => i64 42\n  ]\n]',
    )
  })
})

/* ── 挂上去了没有 ─────────────────────────────────────── */

describe('注册', () => {
  it('protobuf 现在能跑了', () => {
    expect(isImplemented('protobuf')).toBe(true)
  })

  /**
   * **能不能跑与能不能自动识别是两个维度。** protobuf 无魔数，任意字节都可能
   * 是合法 protobuf，进了候选就会把一切都识别成它 —— 所以 `detectable` 实现了
   * 也不能跟着变。见 doc/frontend/codecs.md「识别的三个维度」。
   */
  it('实现了也不能进自动识别的候选集', () => {
    expect(DETECT_CANDIDATES).not.toContain('protobuf')
  })

  it('走 decodeValue 分派下来，一次 IPC 都不发', async () => {
    const value: RedisText = {
      text: null,
      base64: base64FromBytes(bin([tag(1, 0), 0x96, 0x01])),
      totalBytes: null,
    }
    const decoded = await decodeValue('protobuf', value)

    expect(decoded.kind).toBe('struct')
    expect(decoded.kind === 'struct' && decoded.editable).toEqual({ ok: true })
    expect(decoded.kind === 'struct' && decoded.root).toEqual({
      t: 'arr',
      v: [{ t: 'field', no: 1, wire: 0, v: { t: 'int', v: 150n } }],
    })
  })
})
