import { describe, expect, it, vi } from 'vitest'
import { decodeValue, decodeWithRenderer, isImplemented } from './index'
import { phpSerializeCodec } from './php'
import type { Node } from './types'
import { base64FromBytes } from '@/lib/redisText'
import type { RedisText } from '@/types/ipc'

/* `./index` 静态 import 了 `@/api/native`，那条链一路通到 @tauri-apps/api。
   php 这一侧一次 IPC 都不该发，所以整个模块换成假的，顺带钉住"没发过"。 */
vi.mock('@/api/native', () => ({
  codecDecode: vi.fn(),
  codecEncode: vi.fn(),
  codecDetect: vi.fn(),
}))

const encoder = new TextEncoder()

/** 字符串按 UTF-8 编码，数组是原始字节 —— 非 UTF-8 的用例只能这么拼。 */
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
  const node = phpSerializeCodec.decode(input)
  expect(phpSerializeCodec.encode(node)).toEqual(input)
  return node
}

/** 走片 1 的自检，拿到的是"能不能编辑"的结论而不只是解析结果。 */
function verdict(...parts: Array<string | number[]>): { ok: boolean; reason: string } {
  const decoded = decodeWithRenderer(phpSerializeCodec, bin(...parts))
  if (decoded.kind !== 'struct') throw new Error('渲染进程的编解码器只会给出结构')
  return { ok: decoded.editable.ok, reason: decoded.editable.ok ? '' : decoded.editable.reason }
}

function fails(...parts: Array<string | number[]>): void {
  expect(() => phpSerializeCodec.decode(bin(...parts))).toThrow()
}

/* ── 自写这个解析器的全部理由 ─────────────────────────── */

describe('长度是字节数，不是字符数', () => {
  /**
   * `é` 是两个字节。把输入先解成 JS string 再 parse 的库会按 UTF-16 code unit
   * 数出 5，从第 5 个"字符"后面找收尾的 `";`，于是整段错位 —— 自写这个解析器的理由就是这个。
   */
  it('s:6 的 6 数的是 UTF-8 字节', () => {
    expect(roundTrips('s:6:"héllo";')).toEqual({ t: 'str', v: 'héllo' })
  })

  it('按字符数写的长度反而要被拒绝', () => {
    fails('s:5:"héllo";')
  })

  it('宣称的长度超出剩余字节就是坏数据', () => {
    fails('s:10:"abc";')
  })
})

describe('字符串是字节串，不保证是文本', () => {
  it('不是合法 UTF-8 的内容进 bytes，不能被 U+FFFD 污染', () => {
    const node = roundTrips('s:2:"', [0xff, 0xfe], '";')
    expect(node).toEqual({ t: 'bytes', v: Uint8Array.from([0xff, 0xfe]) })
    expect(JSON.stringify(node)).not.toContain('\uFFFD')
  })

  /* TextDecoder 默认会**吃掉**开头的 U+FEFF，解出来少三个字节还一声不吭 */
  it('开头的 BOM 是数据，不是标记', () => {
    const node = roundTrips('s:3:"', [0xef, 0xbb, 0xbf], '";')
    expect(node).toEqual({ t: 'str', v: '\uFEFF' })
  })

  /* private / protected 属性名带 NUL：`\0Cls\0prop` 与 `\0*\0prop` */
  it('属性名里的 NUL 字节要原样留着', () => {
    const node = roundTrips('O:3:"Foo":1:{s:7:"', [0x00], '*', [0x00], 'name";s:3:"bob";}')
    expect(node).toEqual({
      t: 'obj',
      cls: 'Foo',
      v: [[{ t: 'str', v: '\u0000*\u0000name' }, { t: 'str', v: 'bob' }]],
    })
  })
})

/* ── 每种 token ───────────────────────────────────────── */

describe('标量', () => {
  it('N; b: 都能往返', () => {
    expect(roundTrips('N;')).toEqual({ t: 'null' })
    expect(roundTrips('b:0;')).toEqual({ t: 'bool', v: false })
    expect(roundTrips('b:1;')).toEqual({ t: 'bool', v: true })
    fails('b:2;')
  })

  it('i: 是 int64，所以 Node.int 是 bigint', () => {
    expect(roundTrips('i:0;')).toEqual({ t: 'int', v: 0n })
    expect(roundTrips('i:-1;')).toEqual({ t: 'int', v: -1n })
    // 2^53 + 1：number 装不下，JSON 也装不下
    expect(roundTrips('i:9007199254740993;')).toEqual({ t: 'int', v: 9007199254740993n })
    expect(roundTrips('i:9223372036854775807;')).toEqual({ t: 'int', v: 9223372036854775807n })
    expect(roundTrips('i:-9223372036854775808;')).toEqual({ t: 'int', v: -9223372036854775808n })
  })

  it('超出 int64 的 i: 不是 PHP 写得出来的，拒绝掉让识别更可靠', () => {
    fails('i:9223372036854775808;')
  })

  it('d: 的三个特殊字面量单独处理', () => {
    expect(roundTrips('d:INF;')).toEqual({
      t: 'float',
      v: Number.POSITIVE_INFINITY,
      bits: 64,
      literal: 'INF',
    })
    expect(roundTrips('d:-INF;')).toEqual({
      t: 'float',
      v: Number.NEGATIVE_INFINITY,
      bits: 64,
      literal: '-INF',
    })
    expect(roundTrips('d:NAN;')).toEqual({ t: 'float', v: Number.NaN, bits: 64, literal: 'NAN' })
    // PHP 只写 `-INF`，没有 `+INF`
    fails('d:+INF;')
  })

  it('d: 的定点写法', () => {
    expect(roundTrips('d:0.1;')).toEqual({ t: 'float', v: 0.1, bits: 64, literal: '0.1' })
    expect(roundTrips('d:1.5;')).toEqual({ t: 'float', v: 1.5, bits: 64, literal: '1.5' })
    expect(roundTrips('d:0.0001;')).toEqual({ t: 'float', v: 0.0001, bits: 64, literal: '0.0001' })
    expect(roundTrips('d:0;')).toEqual({ t: 'float', v: 0, bits: 64, literal: '0' })
    // -0 与 0 是两串不同的字节，而 String(-0) 给的是 "0"
    const minusZero = roundTrips('d:-0;')
    expect(minusZero.t === 'float' && Object.is(minusZero.v, -0)).toBe(true)
    expect(roundTrips('d:-0.0;')).toEqual({ t: 'float', v: -0, bits: 64, literal: '-0.0' })
  })

  it('d: 的指数写法', () => {
    expect(roundTrips('d:1.0E+100;')).toEqual({ t: 'float', v: 1e100, bits: 64, literal: '1.0E+100' })
    expect(roundTrips('d:1.0E-5;')).toEqual({ t: 'float', v: 1e-5, bits: 64, literal: '1.0E-5' })
    expect(roundTrips('d:-1.5E-7;')).toEqual({
      t: 'float',
      v: -1.5e-7,
      bits: 64,
      literal: '-1.5E-7',
    })
  })

  /**
   * **同一个 double 有多种合法写法，所以解码要把原始字面量带上。**
   *
   * `serialize_precision` 是运行时可配的 ini，决定写几位有效数字。下面两行是
   * `docker run --rm php:{5.6,7.4}-cli` 实测的**真实输出**，同一个值：
   *
   *   5.6（默认 17）      d:0.10000000000000001;   d:1.0000000000000001E-5;
   *   7.4 / 8.3（默认 -1）  d:0.1;                   d:1.0E-5;
   *
   * 从 `number` 重建只能顾一头，另一头就被判成不可编辑。见 types.ts 的 `literal`。
   */
  it('两种 serialize_precision 写的同一个值都写得回去', () => {
    expect(roundTrips('d:0.1;')).toEqual({ t: 'float', v: 0.1, bits: 64, literal: '0.1' })
    expect(roundTrips('d:0.10000000000000001;')).toEqual({
      t: 'float',
      v: 0.1,
      bits: 64,
      literal: '0.10000000000000001',
    })
    expect(roundTrips('d:1.0E-5;')).toEqual({ t: 'float', v: 1e-5, bits: 64, literal: '1.0E-5' })
    expect(roundTrips('d:1.0000000000000001E-5;')).toEqual({
      t: 'float',
      v: 1e-5,
      bits: 64,
      literal: '1.0000000000000001E-5',
    })
    expect(verdict('a:1:{s:5:"ratio";d:0.10000000000000001;}').ok).toBe(true)
  })

  /**
   * 整数值的 float 实测写的是 `d:100;`，**不是** `d:100.0;`——
   * `zero_frac` 是 `var_export` / `json_encode` 那条路径的行为，`serialize()` 不走它。
   * `d:100.0;` 仍然是 `unserialize()` 认的合法输入，所以照样要能往返。
   */
  it('整数值的 float 两种写法都认', () => {
    expect(roundTrips('d:100;')).toEqual({ t: 'float', v: 100, bits: 64, literal: '100' })
    expect(roundTrips('d:100.0;')).toEqual({ t: 'float', v: 100, bits: 64, literal: '100.0' })
  })

  /** 定点/指数的分界实测落在 1e16 与 1e17 之间，三个版本一致。 */
  it('定点与指数的分界', () => {
    expect(roundTrips('d:10000000000000000;')).toEqual({
      t: 'float',
      v: 1e16,
      bits: 64,
      literal: '10000000000000000',
    })
    expect(roundTrips('d:1.0E+17;')).toEqual({ t: 'float', v: 1e17, bits: 64, literal: '1.0E+17' })
    // 用户新写的值走重建路径，那时排版规则才承重
    expect(phpSerializeCodec.encode({ t: 'float', v: 1e16, bits: 64 })).toEqual(
      bin('d:10000000000000000;'),
    )
    expect(phpSerializeCodec.encode({ t: 'float', v: 1e17, bits: 64 })).toEqual(bin('d:1.0E+17;'))
  })

  /**
   * `literal` 是解码那一刻记下的，用户改了值它就过时了 —— 那时必须丢掉重新格式化，
   * 否则会写出一个**错误的值**。这是"原样写回"不会变成捷径的第一道保证。
   */
  it('过时的 literal 不算数，按值重新格式化', () => {
    const stale: Node = { t: 'float', v: 2.5, bits: 64, literal: '0.1' }
    expect(phpSerializeCodec.encode(stale)).toEqual(bin('d:2.5;'))
  })

  /**
   * 第二道保证：`literal` 会被**原样拼进输出**，而 `Node` 可以由编辑路径重建，
   * 不能假设它只来自我们自己的解析器。文法校验挡的是往输出里注入额外的序列化数据。
   */
  it('literal 必须仍然是一个纯粹的浮点字面量', () => {
    const injected: Node = { t: 'float', v: 1, bits: 64, literal: '1;s:4:"evil";' }
    expect(phpSerializeCodec.encode(injected)).toEqual(bin('d:1;'))
  })

  it('d: 只认数字文法，不认 Number() 多认的那些', () => {
    fails('d:0x10;')
    fails('d:Infinity;')
    fails('d:;')
  })
})

describe('数组是有序的 int/string 混合表', () => {
  it('空数组', () => {
    expect(roundTrips('a:0:{}')).toEqual({ t: 'map', v: [] })
  })

  /* 用 map 而不是 arr / Record 的理由就在这条里：key 有类型，而且有顺序 */
  it('key 的类型与顺序都留住', () => {
    expect(roundTrips('a:3:{i:1;s:1:"b";i:0;s:1:"a";s:3:"key";N;}')).toEqual({
      t: 'map',
      v: [
        [{ t: 'int', v: 1n }, { t: 'str', v: 'b' }],
        [{ t: 'int', v: 0n }, { t: 'str', v: 'a' }],
        [{ t: 'str', v: 'key' }, { t: 'null' }],
      ],
    })
  })

  /* PHP 的 unserialize 会把 "5" 这种 key 归一成 int 5。那是数组的语义不是格式的
     语义，跟着归一就再也编不回原字节了 */
  it('数字字符串 key 不归一成整数', () => {
    expect(roundTrips('a:1:{s:1:"5";N;}')).toEqual({
      t: 'map',
      v: [[{ t: 'str', v: '5' }, { t: 'null' }]],
    })
  })

  it('PHP 数组的 key 只能是整数或字符串', () => {
    fails('a:1:{d:1.5;i:0;}')
    fails('a:1:{N;i:0;}')
  })

  it('嵌套', () => {
    expect(roundTrips('a:2:{s:1:"a";a:1:{i:0;s:1:"x";}s:1:"b";i:7;}')).toEqual({
      t: 'map',
      v: [
        [
          { t: 'str', v: 'a' },
          { t: 'map', v: [[{ t: 'int', v: 0n }, { t: 'str', v: 'x' }]] },
        ],
        [{ t: 'str', v: 'b' }, { t: 'int', v: 7n }],
      ],
    })
  })

  it('声明的项数与实际对不上就是坏数据', () => {
    fails('a:2:{i:0;i:1;}')
    fails('a:1:{i:0;i:1;i:2;i:3;}')
  })
})

describe('对象与 enum', () => {
  it('O: 解出类名与属性，不实例化任何东西', () => {
    expect(roundTrips('O:8:"stdClass":1:{s:2:"id";i:7;}')).toEqual({
      t: 'obj',
      cls: 'stdClass',
      v: [[{ t: 'str', v: 'id' }, { t: 'int', v: 7n }]],
    })
  })

  it('带命名空间的类名', () => {
    expect(roundTrips('O:14:"App\\Model\\User":0:{}')).toEqual({
      t: 'obj',
      cls: 'App\\Model\\User',
      v: [],
    })
  })

  /* PHP 8.1 的 enum。`Suit::Hearts` 正是 PHP 源码里写这个 case 的样子，
     而类名不可能含冒号，所以这个记法跟真类名不会撞车 */
  it('E: 记成 PHP 自己写 enum case 的样子', () => {
    expect(roundTrips('E:11:"Suit:Hearts";')).toEqual({
      t: 'obj',
      cls: 'Suit::Hearts',
      v: [],
    })
  })

  it('E: 没有冒号就不是 enum 名', () => {
    fails('E:4:"Suit";')
  })

  /* 一个名叫 C 的 enum 解出来是 `C::X`，别把它当成 C: 的自定义序列化 */
  it('名叫 C 的 enum 不会被当成 C:', () => {
    expect(roundTrips('E:3:"C:X";')).toEqual({ t: 'obj', cls: 'C::X', v: [] })
  })
})

/* ── 严格：解完必须一个字节都不剩 ─────────────────────── */

describe('严格性', () => {
  /**
   * 这不是洁癖：`detectCodec` 拿 decode 抛不抛错当自动识别的判据，而 php 是唯一
   * 进候选集的序列化格式。宽松的解析器会把 `a:1:{i:0;i:1;}` 后面跟着的任意垃圾
   * 也判成成功，自动识别就废了。
   */
  it('尾部有多余字节要抛错', () => {
    fails('N;N;')
    fails('i:1; ')
    fails('a:1:{i:0;i:1;}x')
  })

  it('截断的输入要抛错', () => {
    fails('')
    fails('i:1')
    fails('s:3:"abc"')
    fails('a:1:{i:0;')
    fails('O:3:"Foo":1:{s:2:"id";')
  })

  it('不认识的 token 要抛错', () => {
    fails([0x1f, 0x8b, 0x08, 0x00]) // gzip
    fails('X:1;')
    fails('{"json": true}')
  })

  it('错误信息说得出位置', () => {
    expect(() => phpSerializeCodec.decode(bin('a:1:{i:0;N;}!'))).toThrow('偏移 12')
  })
})

/* ── 不可往返的那几种，判定要走片 1 的自检 ────────────── */

describe('往返自检的结论', () => {
  it('正常的值可以编辑', () => {
    expect(verdict('a:1:{s:1:"a";i:1;}').ok).toBe(true)
  })

  /**
   * `R:` / `r:` 的编号指向序列化时的第几个值，取决于当时的遍历顺序 ——
   * 结构一改编号就指错。编码器主动抛错，好让只读理由说得出**为什么**，
   * 而不是含糊的一句"字节对不上"。
   */
  it('R: / r: 引用判成只读，理由点名遍历顺序', () => {
    for (const input of ['a:2:{i:0;s:3:"abc";i:1;R:2;}', 'a:2:{i:0;O:3:"Foo":0:{}i:1;r:2;}']) {
      const result = verdict(input)
      expect(result.ok).toBe(false)
      expect(result.reason).toContain('遍历顺序')
    }
  })

  /* C: 的内容是类自己写的字节。能原样搬回去，但不知道改动之后对那个类还成不成立 */
  it('C: 判成只读，类名与原始字节都还看得见', () => {
    const node = phpSerializeCodec.decode(bin('C:3:"Foo":4:{abcd}'))
    expect(node).toEqual({
      t: 'obj',
      cls: 'C:Foo',
      v: [[{ t: 'str', v: '__serialized' }, { t: 'bytes', v: encoder.encode('abcd') }]],
    })

    const result = verdict('C:3:"Foo":4:{abcd}')
    expect(result.ok).toBe(false)
    expect(result.reason).toContain('不自描述')
  })

  /**
   * `S:` 带 `\xx` 转义，长度数的是**解码后**的字节数。PHP 自己的 serialize
   * 从不写 `S:`（只写 `s:`），所以我们重编成 `s:` 之后字节对不上 —— 这是对的。
   */
  it('S: 解得开，但重编成 s: 所以判只读', () => {
    expect(phpSerializeCodec.decode(bin('S:6:"h\\C3\\A9llo";'))).toEqual({ t: 'str', v: 'héllo' })
    expect(verdict('S:6:"h\\C3\\A9llo";').ok).toBe(false)
  })

  /* var_unserializer.re 的 iv / uiv 都允许一个前导 `+`，serialize 却从不写 */
  it('前导 + 解得开，但重编时会掉，所以判只读', () => {
    expect(phpSerializeCodec.decode(bin('i:+5;'))).toEqual({ t: 'int', v: 5n })
    expect(verdict('i:+5;').ok).toBe(false)
    expect(verdict('s:+3:"abc";').ok).toBe(false)
  })
})

/* ── 挂上去了没有 ─────────────────────────────────────── */

describe('注册', () => {
  it('phpSerialize 现在能跑了', () => {
    expect(isImplemented('phpSerialize')).toBe(true)
  })

  it('走 decodeValue 分派下来，一次 IPC 都不发', async () => {
    const value: RedisText = {
      text: null,
      base64: base64FromBytes(bin('a:1:{s:1:"a";b:1;}')),
      totalBytes: null,
    }
    const decoded = await decodeValue('phpSerialize', value)

    expect(decoded.kind).toBe('struct')
    expect(decoded.kind === 'struct' && decoded.editable).toEqual({ ok: true })
    expect(decoded.kind === 'struct' && decoded.root).toEqual({
      t: 'map',
      v: [[{ t: 'str', v: 'a' }, { t: 'bool', v: true }]],
    })
  })
})
