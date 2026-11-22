import { describe, expect, it } from 'vitest'
import { renderNode } from './render'
import type { Node } from './types'

describe('展示文本', () => {
  /* 这四条正是 `Node` 存在的理由：JSON 装不下它们，展示文本就必须看得见它们 */

  it('int64 标出来，而且精度不丢', () => {
    // 2^53 + 1：JSON.parse 会把它变成 9007199254740992
    expect(renderNode({ t: 'int', v: 9007199254740993n })).toBe('i64 9007199254740993')
  })

  it('float 标出宽度，32 与 64 分得开', () => {
    expect(renderNode({ t: 'float', v: 1.5, bits: 32 })).toBe('f32 1.5')
    expect(renderNode({ t: 'float', v: 1.5, bits: 64 })).toBe('f64 1.5')
    // -0 与 0 在 msgpack / PHP 里是不同的字节
    expect(renderNode({ t: 'float', v: -0, bits: 64 })).toBe('f64 -0')
  })

  it('字节列十六进制，并带上真实长度', () => {
    expect(renderNode({ t: 'bytes', v: Uint8Array.from([0xde, 0xad, 0x00]) })).toBe(
      'bytes(3) de ad 00',
    )
    expect(renderNode({ t: 'bytes', v: new Uint8Array() })).toBe('bytes(0)')
  })

  it('长字节只列前面一截，但 bytes(n) 报的是真实长度', () => {
    const rendered = renderNode({ t: 'bytes', v: new Uint8Array(200) })
    expect(rendered.startsWith('bytes(200) ')).toBe(true)
    expect(rendered.endsWith(' …')).toBe(true)
  })

  it('map 的 key 可以不是字符串，而且看得出是什么类型', () => {
    const node: Node = {
      t: 'map',
      v: [
        [{ t: 'str', v: 'name' }, { t: 'str', v: 'alice' }],
        [{ t: 'int', v: 7n }, { t: 'bool', v: true }],
      ],
    }
    expect(renderNode(node)).toBe('{\n  "name" => "alice"\n  i64 7 => true\n}')
  })

  it('类名摆在前面 —— 它是解出来的事实', () => {
    const node: Node = {
      t: 'obj',
      cls: 'App\\Model\\User',
      v: [[{ t: 'str', v: 'id' }, { t: 'int', v: 1n }]],
    }
    expect(renderNode(node)).toBe('App\\Model\\User {\n  "id" => i64 1\n}')
  })

  /**
   * protobuf 无 schema 时只有字段号与 wire type，就照实摆字段号与 wire type。
   *
   * wire type 2 装的是 string / bytes / 嵌套 message / packed repeated 中的哪一种
   * **只能猜**，所以要把猜测显式标注出来（doc/frontend/codec-formats.md「Protobuf」）
   * —— 用户得看见这是决策而不是
   * 事实。标注推导自子节点的类型，见 `render.ts` 的 `guessTag`。
   */
  it('protobuf 的字段摆出字段号与 wire type，猜测另外标注', () => {
    expect(renderNode({ t: 'field', no: 2, wire: 2, v: { t: 'str', v: 'testing' } })).toBe(
      '#2 len => ~str "testing"',
    )
    expect(renderNode({ t: 'field', no: 2, wire: 2, v: { t: 'arr', v: [] } })).toBe(
      '#2 len => ~msg []',
    )
    expect(
      renderNode({ t: 'field', no: 2, wire: 2, v: { t: 'bytes', v: Uint8Array.from([0xff]) } }),
    ).toBe('#2 len => ~bytes bytes(1) ff')
    // 认不出来的 wire type 退回数字：`Node` 可以由编辑路径重建
    expect(renderNode({ t: 'field', no: 2, wire: 4, v: { t: 'null' } })).toBe('#2 wire=4 => null')
  })

  /**
   * 定宽的那两种 wire type 收的是**原始位**（`fixed64` / `sfixed64` / `double`
   * 共用 wire 1，无 schema 时分不出来，见 `protobuf.ts`），所以另一种读法必须
   * 由展示层重解释出来 —— 展示决策就该待在这里（doc/backend/codec.md「归属切分」）。
   */
  it('定宽字段与大 varint 给出另一种读法', () => {
    expect(renderNode({ t: 'field', no: 1, wire: 5, v: { t: 'int', v: 1065353216n } })).toBe(
      '#1 fixed32 => i64 1065353216 (float 1)',
    )
    expect(
      renderNode({ t: 'field', no: 1, wire: 1, v: { t: 'int', v: 4607182418800017408n } }),
    ).toBe('#1 fixed64 => i64 4607182418800017408 (double 1)')
    // raw ≥ 2^63 的 varint 十有八九是个负的 int64
    expect(
      renderNode({ t: 'field', no: 1, wire: 0, v: { t: 'int', v: 18446744073709551615n } }),
    ).toBe('#1 varint => i64 18446744073709551615 (int64 -1)')
    // zigzag 对每个值都有一种读法，显示出来是噪声不是信息
    expect(renderNode({ t: 'field', no: 1, wire: 0, v: { t: 'int', v: 1n } })).toBe(
      '#1 varint => i64 1',
    )
  })

  it('msgpack ext 带上 type code', () => {
    expect(renderNode({ t: 'ext', code: -1, v: Uint8Array.from([0x5a, 0x4d]) })).toBe(
      'ext(-1) bytes(2) 5a 4d',
    )
  })

  it('不是 JSON，所以用 => 而不是 : 且不加逗号', () => {
    const node: Node = {
      t: 'arr',
      v: [{ t: 'null' }, { t: 'str', v: 'a' }],
    }
    expect(renderNode(node)).toBe('[\n  null\n  "a"\n]')
  })

  it('空容器不占三行', () => {
    expect(renderNode({ t: 'arr', v: [] })).toBe('[]')
    expect(renderNode({ t: 'map', v: [] })).toBe('{}')
    expect(renderNode({ t: 'obj', cls: 'Foo', v: [] })).toBe('Foo {}')
  })

  it('字符串里的控制字符要转义，不能把布局搅乱', () => {
    expect(renderNode({ t: 'str', v: 'a\nb' })).toBe('"a\\nb"')
  })

  it('超长容器只展开一部分，并把丢掉多少说出来', () => {
    const v: Node[] = []
    for (let i = 0; i < 2500; i += 1) v.push({ t: 'int', v: BigInt(i) })
    const rendered = renderNode({ t: 'arr', v })
    expect(rendered).toContain('… 还有 500 项未显示')
  })

  /* 深度上限兼作环路保护：PHP 的 R: / r: 让解析器可能产出共享子树 */
  it('深到一定程度就停，不炸栈', () => {
    let node: Node = { t: 'int', v: 0n }
    for (let i = 0; i < 200; i += 1) node = { t: 'arr', v: [node] }
    expect(renderNode(node)).toContain('嵌套过深')
  })
})
