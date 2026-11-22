import { beforeEach, describe, expect, it, vi } from 'vitest'
import { base64FromBytes } from '@/lib/redisText'
import { DETECT_CANDIDATES } from '@/lib/codecs'
import type { RedisText } from '@/types/ipc'
import { decodeValue, detectCodec, isImplemented, unimplementedReason } from './index'
import { javaSerializeCodec } from './java'
import { renderNode } from './render'
import type { JavaClassData, JavaContent, JavaValue, Node } from './types'

const native = vi.hoisted(() => ({
  codecDecode: vi.fn(),
  codecDetect: vi.fn(),
}))
vi.mock('@/api/native', () => native)

beforeEach(() => vi.clearAllMocks())

/**
 * 下面 19 组主夹具全部由 `scripts/dev/GenerateJavaCodecFixtures.java` 在真实 JDK 21
 * 的 `ObjectOutputStream` 上生成；不是照本解析器反推的。运行那个 source launcher
 * 会打印名字、字节数和完整 hex，便于以后换 JDK 复核。
 */
const FIXTURES = {
  modifiedUtfString: 'aced000574000b41c080c3a9eda0bdedb880',
  primitiveArray:
    'aced0005757200025b494dba602676eab2a5020000787000000004ffffffff000000000000002a7fffffff',
  objectArray:
    'aced0005757200135b4c6a6176612e6c616e672e537472696e673badd256e7e91d7b470200007870000000037400046c656674707400057269676874',
  inheritance:
    'aced00057372002547656e65726174654a617661436f6465634669787475726573244368696c645265636f726400000000000000010200024900066e756d6265724c00056c6162656c7400124c6a6176612f6c616e672f537472696e673b7872002447656e65726174654a617661436f646563466978747572657324426173655265636f726400000000000000010200024900066e756d6265724c00056c6162656c71007e000178700000000774000462617365000000097400056368696c64',
  hashMap:
    'aced0005737200116a6176612e7574696c2e486173684d61700507dac1c31660d103000246000a6c6f6164466163746f724900097468726573686f6c6478703f4000000000000c77080000001000000002740005616c706861737200116a6176612e6c616e672e496e746567657212e2a0a4f781873802000149000576616c7565787200106a6176612e6c616e672e4e756d62657286ac951d0b94e08b020000787000000001740004626574617371007e00030000000278',
  enumValue:
    'aced00057e72001f47656e65726174654a617661436f646563466978747572657324436f6c6f7200000000000000001200007872000e6a6176612e6c616e672e456e756d00000000000000001200007870740005475245454e',
  classValue:
    'aced0005767200106a6176612e6c616e672e537472696e67a0f0a4387a3bb3420200007870',
  multiDimensionalArray:
    'aced0005757200145b5b4c6a6176612e6c616e672e537472696e673b324d09ad8432e457020000787000000002757200135b4c6a6176612e6c616e672e537472696e673badd256e7e91d7b470200007870000000017400066e657374656470',
  sharedReference:
    'aced00057372002447656e65726174654a617661436f6465634669787475726573245368617265645061697200000000000000010200024c00046c6566747400124c6a6176612f6c616e672f4f626a6563743b4c0005726967687471007e0001787074000673686172656471007e0003',
  selfReference:
    'aced00057372002447656e65726174654a617661436f64656346697874757265732453656c665265636f726400000000000000010200024c00046e616d657400124c6a6176612f6c616e672f537472696e673b4c000473656c667400264c47656e65726174654a617661436f64656346697874757265732453656c665265636f72643b7870740004726f6f7471007e0003',
  multipleTopLevel:
    'aced00057400056669727374757200025b494dba602676eab2a50200007870000000020000000100000002',
  reset: 'aced0005740005616761696e71007e000079740005616761696e',
  customWriteObject:
    'aced00057372002647656e65726174654a617661436f646563466978747572657324437573746f6d5265636f72640000000000000001030002490005636f756e744c00046e616d657400124c6a6176612f6c616e672f537472696e673b787000000003740006637573746f6d770a1020304000047461696c7372002647656e65726174654a617661436f6465634669787475726573244e65737465645265636f726400000000000000010200014c000576616c756571007e00017870740006696e7369646578',
  customWithoutDefaultFields:
    'aced00057372002947656e65726174654a617661436f6465634669787475726573244e6f44656661756c745265636f72640000000000000001030002490005636f756e744c00046e616d657400124c6a6176612f6c616e672f537472696e673b78707704112233447372002647656e65726174654a617661436f6465634669787475726573244e65737465645265636f726400000000000000010200014c000576616c756571007e0001787074000b6f6e6c792d637573746f6d78',
  customWithInterleavedDefaultFields:
    'aced00057372003247656e65726174654a617661436f646563466978747572657324496e7465726c656176656444656661756c745265636f72640000000000000001030002490005636f756e744c00046e616d657400124c6a6176612f6c616e672f537472696e673b78707704010203047372002647656e65726174654a617661436f6465634669787475726573244e65737465645265636f726400000000000000010200014c000576616c756571007e000178707400066265666f7265000000057400066d6964646c65770811121314151617187371007e0003740005616674657278',
  externalizableProtocol2:
    'aced00057372002847656e65726174654a617661436f64656346697874757265732445787465726e616c5265636f726400000000000000010c00007870770e50607080000865787465726e616c7372002647656e65726174654a617661436f6465634669787475726573244e65737465645265636f726400000000000000010200014c000576616c75657400124c6a6176612f6c616e672f537472696e673b78707400066e657374656478',
  externalizableProtocol1:
    'aced00057372002847656e65726174654a617661436f64656346697874757265732445787465726e616c5265636f72640000000000000001040000787050607080000865787465726e616c7372002647656e65726174654a617661436f6465634669787475726573244e65737465645265636f726400000000000000010200014c000576616c75657400124c6a6176612f6c616e672f537472696e673b78707400066e6573746564',
  dynamicProxy:
    'aced0005737d00000001002f47656e65726174654a617661436f6465634669787475726573244669787475726550726f7879496e74657266616365787200176a6176612e6c616e672e7265666c6563742e50726f7879e127da20cc1043cb0200014c0001687400254c6a6176612f6c616e672f7265666c6563742f496e766f636174696f6e48616e646c65723b78707372003747656e65726174654a617661436f64656346697874757265732453657269616c697a61626c65496e766f636174696f6e48616e646c657200000000000000010200014c00056c6162656c7400124c6a6176612f6c616e672f537472696e673b787074000d70726f78792d68616e646c6572',
  fatalException:
    'aced00057372002747656e65726174654a617661436f6465634669787475726573244661696c696e675265636f726400000000000000010300014900066d61726b657278700000000777042122232474000e6265666f72652d6661696c7572657b737200136a6176612e696f2e494f457863657074696f6e6c8073646525f0ab020000787200136a6176612e6c616e672e457863657074696f6ed0fd1f3e1a3b1cc4020000787200136a6176612e6c616e672e5468726f7761626c65d5c635273977b8cb0300044c000563617573657400154c6a6176612f6c616e672f5468726f7761626c653b4c000d64657461696c4d6573736167657400124c6a6176612f6c616e672f537472696e673b5b000a737461636b547261636574001e5b4c6a6176612f6c616e672f537461636b5472616365456c656d656e743b4c001473757070726573736564457863657074696f6e737400104c6a6176612f7574696c2f4c6973743b78707074000f66697874757265206661696c7572657572001e5b4c6a6176612e6c616e672e537461636b5472616365456c656d656e743b02462a3c3cfd22390200007870000000007372001f6a6176612e7574696c2e436f6c6c656374696f6e7324456d7074794c6973747ab817b43ca79ede020000787078',
} as const

function bytes(hex: string): Uint8Array {
  if (hex.length % 2 !== 0) throw new Error('test hex 长度不是偶数')
  const out = new Uint8Array(hex.length / 2)
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16)
  return out
}

function source(hex: string): RedisText {
  return { text: null, base64: base64FromBytes(bytes(hex)), totalBytes: null }
}

function decode(hex: string): Node {
  return javaSerializeCodec.decode(bytes(hex))
}

function javaValue(hex: string): JavaValue {
  const node = decode(hex)
  if (node.t !== 'java') throw new Error(`expected java node, got ${node.t}`)
  return node.v
}

function objectValue(hex: string): Extract<JavaValue, { kind: 'object' }> {
  const value = javaValue(hex)
  if (value.kind !== 'object') throw new Error(`expected object, got ${value.kind}`)
  return value
}

function field(data: JavaClassData, name: string): Node {
  const found = data.fields.find((item) => item.descriptor.name === name)
  if (!found) throw new Error(`field ${name} not found`)
  return found.value
}

function contentValue(content: JavaContent): Node {
  if (content.kind !== 'value') throw new Error(`expected value content, got ${content.kind}`)
  return content.value
}

describe('Java stream 与基础值', () => {
  it('解 JDK Modified UTF-8：NUL、BMP 与 surrogate pair 都保留', () => {
    expect(decode(FIXTURES.modifiedUtfString)).toEqual({ t: 'str', v: 'A\0é😀' })
  })

  it('reader 兼容 JDK 接受的单字节 NUL 与 overlong 组', () => {
    expect(decode('aced000574000100')).toEqual({ t: 'str', v: '\0' })
    expect(decode('aced0005740002c181')).toEqual({ t: 'str', v: 'A' })
    expect(decode('aced0005740003e08181')).toEqual({ t: 'str', v: 'A' })
  })

  it('header-only 空流保留成空 stream，而不是假造根值', () => {
    expect(javaValue('aced0005')).toEqual({ kind: 'stream', contents: [] })
  })

  it('多个顶层 content 一直读到 EOF 并保留顺序', () => {
    const stream = javaValue(FIXTURES.multipleTopLevel)
    expect(stream.kind).toBe('stream')
    if (stream.kind !== 'stream') return
    expect(contentValue(stream.contents[0])).toEqual({ t: 'str', v: 'first' })
    const second = contentValue(stream.contents[1])
    expect(second.t === 'java' && second.v.kind).toBe('primitiveArray')
  })

  it('顶层 primitive write 以 blockdata content 保留', () => {
    const stream = javaValue('aced0005770401020304')
    expect(stream).toEqual({
      kind: 'stream',
      contents: [{ kind: 'block', value: bytes('01020304') }],
    })
  })

  it('primitive array 保留 descriptor、项数与原始大端字节', () => {
    const value = javaValue(FIXTURES.primitiveArray)
    expect(value.kind).toBe('primitiveArray')
    if (value.kind !== 'primitiveArray') return
    expect(value).toMatchObject({ className: '[I', component: 'I', count: 4 })
    expect(renderNode({ t: 'java', v: value })).toContain('i32 -1')
    expect(renderNode({ t: 'java', v: value })).toContain('i32 2147483647')
  })

  it('object array 的 null 与 String 都是独立对象 token', () => {
    const value = javaValue(FIXTURES.objectArray)
    expect(value.kind).toBe('array')
    if (value.kind !== 'array') return
    expect(value.className).toBe('[Ljava.lang.String;')
    expect(value.values).toEqual([{ t: 'str', v: 'left' }, { t: 'null' }, { t: 'str', v: 'right' }])
  })

  it('Class 与多维对象数组走各自的 TC_CLASS / TC_ARRAY 结构', () => {
    const classValue = javaValue(FIXTURES.classValue)
    expect(classValue.kind).toBe('class')
    if (classValue.kind === 'class') {
      expect(classValue.classes[classValue.classes.length - 1].name).toBe('java.lang.String')
    }

    const outer = javaValue(FIXTURES.multiDimensionalArray)
    expect(outer.kind).toBe('array')
    if (outer.kind !== 'array') return
    expect(outer.className).toBe('[[Ljava.lang.String;')
    expect(outer.values[1]).toEqual({ t: 'null' })
    const inner = outer.values[0]
    expect(inner.t === 'java' && inner.v.kind).toBe('array')
    if (inner.t === 'java' && inner.v.kind === 'array') {
      expect(inner.v.values).toEqual([{ t: 'str', v: 'nested' }])
    }
  })
})

describe('class descriptor 与 classdata', () => {
  it('父类到子类分层读取，同名字段不会拍平覆盖', () => {
    const value = objectValue(FIXTURES.inheritance)
    expect(value.classes.map((item) => item.descriptor.name)).toEqual([
      expect.stringMatching(/BaseRecord$/),
      expect.stringMatching(/ChildRecord$/),
    ])
    expect(field(value.classes[0], 'number')).toEqual({ t: 'int', v: 7n })
    expect(field(value.classes[0], 'label')).toEqual({ t: 'str', v: 'base' })
    expect(field(value.classes[1], 'number')).toEqual({ t: 'int', v: 9n })
    expect(field(value.classes[1], 'label')).toEqual({ t: 'str', v: 'child' })
  })

  it('HashMap 只机械展示默认字段、opaque header 与嵌套 key/value', () => {
    const value = objectValue(FIXTURES.hashMap)
    const map = value.classes[0]
    expect(map.descriptor.name).toBe('java.util.HashMap')
    expect(field(map, 'loadFactor')).toEqual({ t: 'float', v: 0.75, bits: 32 })
    expect(field(map, 'threshold')).toEqual({ t: 'int', v: 12n })
    expect(map.custom[0]).toEqual({ kind: 'block', value: bytes('0000001000000002') })
    expect(contentValue(map.custom[1])).toEqual({ t: 'str', v: 'alpha' })
    expect(contentValue(map.custom[3])).toEqual({ t: 'str', v: 'beta' })

    const rendered = renderNode({ t: 'java', v: value })
    expect(rendered).toContain('SC_WRITE_METHOD')
    expect(rendered).toContain('blockdata bytes(8) 00 00 00 10 00 00 00 02')
  })

  it('常规 writeObject 读取默认字段，再保留 custom block/object', () => {
    const value = objectValue(FIXTURES.customWriteObject)
    const data = value.classes[0]
    expect(field(data, 'count')).toEqual({ t: 'int', v: 3n })
    expect(field(data, 'name')).toEqual({ t: 'str', v: 'custom' })
    expect(data.defaultFieldBoundary).toBe(0)
    expect(data.custom.map((item) => item.kind)).toEqual(['block', 'value'])
  })

  it('writeObject 完全不调用 defaultWriteObject 时不伪造字段', () => {
    const value = objectValue(FIXTURES.customWithoutDefaultFields)
    expect(value.classes[0].fields).toEqual([])
    expect(value.classes[0].defaultFieldBoundary).toBeNull()
    expect(value.classes[0].custom.map((item) => item.kind)).toEqual(['block', 'value'])
  })

  it('defaultWriteObject 在 custom contents 中间时枚举唯一边界', () => {
    const value = objectValue(FIXTURES.customWithInterleavedDefaultFields)
    const data = value.classes[0]
    expect(field(data, 'count')).toEqual({ t: 'int', v: 5n })
    expect(field(data, 'name')).toEqual({ t: 'str', v: 'middle' })
    expect(data.defaultFieldBoundary).toBe(2)
    expect(data.custom.map((item) => item.kind)).toEqual(['block', 'value', 'block', 'value'])

    const rendered = renderNode({ t: 'java', v: value })
    expect(rendered.indexOf('"before"')).toBeLessThan(rendered.indexOf('I count'))
    expect(rendered.indexOf('I count')).toBeLessThan(rendered.indexOf('"after"'))
  })

  it('protocol 2 Externalizable 只读最派生类的一段 annotation', () => {
    const value = objectValue(FIXTURES.externalizableProtocol2)
    const leaf = value.classes[value.classes.length - 1]
    expect(leaf.fields).toEqual([])
    expect(leaf.custom.map((item) => item.kind)).toEqual(['block', 'value'])
    expect(leaf.descriptor.flags).toBe(0x0c)
  })

  it('protocol 1 Externalizable 没有边界，明确拒绝而不是猜', () => {
    expect(() => decode(FIXTURES.externalizableProtocol1)).toThrow('protocol 1')
  })
})

describe('handle、reset 与特殊对象', () => {
  it('共享引用保留 wire handle，不展开成重复子树', () => {
    const data = objectValue(FIXTURES.sharedReference).classes[0]
    expect(field(data, 'left')).toEqual({ t: 'str', v: 'shared' })
    expect(field(data, 'right')).toMatchObject({
      t: 'java',
      v: { kind: 'reference', epoch: 0, handle: 0x7e0003, target: 'String "shared"' },
    })
  })

  it('自引用只生成 reference 节点，不造环', () => {
    const data = objectValue(FIXTURES.selfReference).classes[0]
    expect(field(data, 'self')).toMatchObject({
      t: 'java',
      v: {
        kind: 'reference',
        epoch: 0,
        handle: 0x7e0003,
        target: expect.stringMatching(/object .*SelfRecord$/),
      },
    })
  })

  it('TC_RESET 后 handle 重新编号，但 epoch 不同', () => {
    const stream = javaValue(FIXTURES.reset)
    expect(stream.kind).toBe('stream')
    if (stream.kind !== 'stream') return
    expect(contentValue(stream.contents[1])).toMatchObject({
      t: 'java',
      v: { kind: 'reference', epoch: 0, handle: 0x7e0000, target: 'String "again"' },
    })
    expect(stream.contents[2]).toEqual({ kind: 'reset', epoch: 1 })
    expect(contentValue(stream.contents[3])).toEqual({ t: 'str', v: 'again' })
  })

  it('enum 保留类型与常量名，不实例化枚举', () => {
    const value = javaValue(FIXTURES.enumValue)
    expect(value.kind).toBe('enum')
    if (value.kind !== 'enum') return
    expect(value.className).toMatch(/Color$/)
    expect(value.name).toEqual({ t: 'str', v: 'GREEN' })
  })

  it('dynamic proxy 先读 Proxy.h，再保留 interface layer', () => {
    const value = objectValue(FIXTURES.dynamicProxy)
    expect(value.className).toMatch(/^proxy\(.*FixtureProxyInterface\)$/)
    expect(value.classes).toHaveLength(2)
    expect(value.classes[0].descriptor.name).toBe('java.lang.reflect.Proxy')
    const handler = field(value.classes[0], 'h')
    expect(handler.t === 'java' && handler.v.kind).toBe('object')
    if (handler.t !== 'java' || handler.v.kind !== 'object') return
    expect(field(handler.v.classes[0], 'label')).toEqual({ t: 'str', v: 'proxy-handler' })
    expect(value.classes[1].descriptor).toMatchObject({
      kind: 'proxy',
      interfaces: [expect.stringMatching(/FixtureProxyInterface$/)],
    })
  })

  it('TC_EXCEPTION 丢弃半截外层对象并 unwind 成顶层 exception', () => {
    const value = javaValue(FIXTURES.fatalException)
    expect(value.kind).toBe('exception')
    if (value.kind !== 'exception') return
    expect(value.value.t === 'java' && value.value.v.kind).toBe('object')
    if (value.value.t !== 'java' || value.value.v.kind !== 'object') return
    expect(value.value.v.className).toBe('java.io.IOException')
    const throwable = value.value.v.classes.find(
      (item) => item.descriptor.name === 'java.lang.Throwable',
    )
    expect(throwable?.alternatives).toHaveLength(2)
    const rendered = renderNode({ t: 'java', v: value })
    expect(rendered).toContain('fixture failure')
    expect(rendered).not.toContain('FailingRecord')
  })
})

describe('消费完整性与资源边界', () => {
  it('magic、version、未知 type code 与坏 handle 都拒绝', () => {
    expect(() => decode('00000005')).toThrow('stream magic')
    expect(() => decode('aced0006')).toThrow('stream version')
    expect(() => decode('aced000501')).toThrow('不认识 type code')
    expect(() => decode('aced000571007e0000')).toThrow('不存在')
  })

  it('Modified UTF-8 截断/坏 continuation 拒绝', () => {
    expect(() => decode('aced0005740001c2')).toThrow('没有写完')
    expect(() => decode('aced0005740002c220')).toThrow('续字节')
  })

  it('负 block 长度、负数组长度与缺失 ENDBLOCKDATA 拒绝', () => {
    expect(() => decode('aced00057affffffff')).toThrow('负长度')
    const negativeArrayLength = FIXTURES.primitiveArray.replace(
      '00000004ffffffff00000000',
      'ffffffffffffffff00000000',
    )
    expect(() => decode(negativeArrayLength)).toThrow('负长度')
    expect(() => decode(FIXTURES.hashMap.slice(0, -2))).toThrow('TC_ENDBLOCKDATA')
  })

  it('nested reset、flags=0 对象与非法 flags 组合拒绝', () => {
    const nestedReset = 'aced00057372000141000000000000000103000078707978'
    const flagsZeroObject = 'aced0005737200014100000000000000010000007870'
    const illegalFlags = 'aced00057200014100000000000000011300007870'
    expect(() => decode(nestedReset)).toThrow('TC_RESET 只能出现在顶层')
    expect(() => decode(flagsZeroObject)).toThrow('没有可用的序列化 flags')
    expect(() => decode(illegalFlags)).toThrow('组合 0x13')
  })

  it('未完成 classDesc 自引用与畸形 proxy/array descriptor 拒绝', () => {
    const incompleteClassDesc = 'aced00057200014100000000000000000000007671007e00007870'
    const proxyWithoutSuperclass = 'aced0005737d000000007870'
    const arrayWithField = 'aced0005757200025b49000000000000000002000149000178787000000000'
    expect(() => decode(incompleteClassDesc)).toThrow('尚未写完的 class descriptor')
    expect(() => decode(proxyWithoutSuperclass)).toThrow('java.lang.reflect.Proxy')
    expect(() => decode(arrayWithField)).toThrow('不能有字段或 superclass')
  })

  it('字段顺序与完整 JVM field descriptor 都做机械校验', () => {
    const unsortedFields = 'aced00057200014100000000000000010200024900017a490001617870'
    const badDescriptor =
      'aced00057200014100000000000000010200014c0001787400084c676172626167657870'
    expect(() => decode(unsortedFields)).toThrow('没有按名称升序排列')
    expect(() => decode(badDescriptor)).toThrow('descriptor')
  })

  it('SC_WRITE_METHOD 的等价多解显式保留 alternatives，不偷偷猜一个', () => {
    const ambiguous = 'aced0005737200014100000000000000010300014200017878707078'
    const value = objectValue(ambiguous)
    expect(value.classes[0].alternatives).toHaveLength(2)
    expect(renderNode({ t: 'java', v: value })).toContain('ambiguous-classdata')
  })

  it('默认字段边界枚举与回溯都有硬上限', () => {
    const descriptor = 'aced000573720001410000000000000001030001490001787870'
    const tooManyBoundaries = `${descriptor}${'7700'.repeat(129)}0000000178`
    expect(() => decode(tooManyBoundaries)).toThrow('插入点超过 128')
  })

  it('TC_EXCEPTION payload 至少必须是 TC_OBJECT，不能拿 String 冒充', () => {
    expect(() => decode('aced00057b74000178')).toThrow('TC_OBJECT Throwable')
  })

  it('fatal exception payload 内再次出现 TC_EXCEPTION 说明流已损坏', () => {
    const nestedException = 'aced00057b7372000141000000000000000103000078707b'
    expect(() => decode(nestedException)).toThrow('不能再嵌 TC_EXCEPTION')
  })

  it('TC_LONGSTRING 用 signed i64 长度并保留 lone surrogate', () => {
    expect(decode('aced00057c0000000000000003616263')).toEqual({ t: 'str', v: 'abc' })
    expect(decode('aced0005740003eda080')).toEqual({ t: 'str', v: '\ud800' })
    expect(() => decode('aced00057cffffffffffffffff')).toThrow('负长度')
  })

  it('合法根值后的垃圾会被当下一条 content 检查并拒绝', () => {
    expect(() => decode(`${FIXTURES.modifiedUtfString}01`)).toThrow('不认识 type code')
  })
})

describe('只读注册、自动识别与统一入口', () => {
  it('Java codec 已实现且仍是 noEncoder', async () => {
    expect(isImplemented('javaSerialize')).toBe(true)
    expect(unimplementedReason('javaSerialize')).toBeNull()

    const decoded = await decodeValue('javaSerialize', source(FIXTURES.modifiedUtfString))
    expect(decoded.kind).toBe('struct')
    expect(decoded.kind === 'struct' && decoded.editable).toMatchObject({
      ok: false,
      kind: 'noEncoder',
    })
    expect(native.codecDecode).not.toHaveBeenCalled()
  })

  it('四字节 header 是自动识别前缀，完整解析成功后不问后端', async () => {
    expect(javaSerializeCodec.detectPrefix?.(bytes(FIXTURES.modifiedUtfString))).toBe(true)
    expect(javaSerializeCodec.detectPrefix?.(bytes('aced0004'))).toBe(false)
    expect(DETECT_CANDIDATES).toContain('javaSerialize')

    await expect(detectCodec(source(FIXTURES.modifiedUtfString))).resolves.toBe('javaSerialize')
    expect(native.codecDetect).not.toHaveBeenCalled()
  })
})
