import type { ReadonlyCodec } from './index'
import type {
  JavaClassData,
  JavaClassLayer,
  JavaContent,
  JavaFieldDescriptor,
  Node,
} from './types'

/**
 * Java Object Serialization Stream Protocol 的**只读结构解析器**。
 *
 * 规范来源：Java Object Serialization Specification §6（stream protocol）以及
 * `DataInput.readUTF` 的 Modified UTF-8 定义。这里独立实现 wire grammar，不复制
 * OpenJDK 源码，也绝不调用 `ObjectInputStream`：后者会加载类、实例化对象并执行
 * `readObject` / `readResolve` / `readExternal`，正是这个查看器要避开的风险。
 *
 * 解析器只做三件事：
 *   1. 机械读取 class descriptor、字段、数组、字符串与 handle 图；
 *   2. `SC_WRITE_METHOD` / protocol 2 Externalizable 里的 blockdata 原样展示，
 *      其中带 TC 标签的嵌套对象继续解析；
 *   3. 完整消费输入。Java 流没有 pickle 那样的全局 STOP；header 后可以有多个
 *      content，所以一直读到 Redis 值的末尾，任何不完整的尾巴都会报错。
 *
 * **没有 encode。** 写出 Java serialization 需要类的私有序列化契约，而且写回
 * 一份会被下游 JVM 加载的载荷不在本工具的安全边界内，见
 * doc/backend/codec.md「Readonly 安全边界」；格式事实见
 * doc/frontend/codec-formats.md「Java 序列化」。
 */

/* ── Stream header 与 type code ───────────────────────── */

const STREAM_MAGIC = 0xaced
const STREAM_VERSION = 5
const BASE_WIRE_HANDLE = 0x7e0000

const TC_NULL = 0x70
const TC_REFERENCE = 0x71
const TC_CLASSDESC = 0x72
const TC_OBJECT = 0x73
const TC_STRING = 0x74
const TC_ARRAY = 0x75
const TC_CLASS = 0x76
const TC_BLOCKDATA = 0x77
const TC_ENDBLOCKDATA = 0x78
const TC_RESET = 0x79
const TC_BLOCKDATALONG = 0x7a
const TC_EXCEPTION = 0x7b
const TC_LONGSTRING = 0x7c
const TC_PROXYCLASSDESC = 0x7d
const TC_ENUM = 0x7e

const SC_WRITE_METHOD = 0x01
const SC_SERIALIZABLE = 0x02
const SC_EXTERNALIZABLE = 0x04
const SC_BLOCK_DATA = 0x08
const SC_ENUM = 0x10
const KNOWN_FLAGS =
  SC_WRITE_METHOD | SC_SERIALIZABLE | SC_EXTERNALIZABLE | SC_BLOCK_DATA | SC_ENUM

const PRIMITIVE_TYPES = new Set(['B', 'C', 'D', 'F', 'I', 'J', 'S', 'Z'])
const OBJECT_TYPES = new Set(['L', '['])

/* ── 防御上限 ─────────────────────────────────────────── */

/** 递归 grammar 与下游遍历共用的深度上限。真实业务对象离 256 层很远。 */
const MAX_DEPTH = 256

/** 对象数组 / proxy interface 等需要逐项分配的集合上限。 */
const MAX_ITEMS = 1_000_000

/** Node 与 handle 分开计数：大量 back-reference 不能绕开任意一边的预算。 */
const MAX_NODES = 1_000_000
const MAX_HANDLES = 1_000_000

/** serializable 父类链不应该有环，也不需要有几百层。 */
const MAX_CLASS_CHAIN = 256

/**
 * `SC_WRITE_METHOD` 的默认字段没有 marker，只能枚举它在 annotation content 边界
 * 的插入点。真实实现通常只有个位数 content；上限防止特制输入逼出二次方回溯。
 */
const MAX_DEFAULT_FIELD_BOUNDARIES = 128

/** 两类回溯成本分开计，且都不随 restore：Map 复制防 OOM，重复读 suffix 防 CPU 放大。 */
const MAX_BACKTRACK_HANDLE_COPIES = 2_000_000
const MAX_BACKTRACK_BYTES = 64 * 1024 * 1024

/** 等价布局也不能无限保留；真实 Throwable 是两个。超过说明输入在刻意制造歧义。 */
const MAX_AMBIGUOUS_LAYOUTS = 8

/* ── 内部状态；这些类型不会泄漏到展示 Node ───────────── */

interface ClassDesc {
  layer: JavaClassLayer
  superClass: ClassDesc | null
  complete: boolean
}

type HandlePayload =
  | { kind: 'classDesc'; value: ClassDesc }
  | { kind: 'string'; value: string | null }
  | { kind: 'value'; summary: string }

type HandleEntry = HandlePayload & { origin: number }

interface StringObject {
  node: Node
  value: string
}

interface ParserSnapshot {
  offset: number
  handles: Map<number, HandleEntry>
  nextHandle: number
  epoch: number
  nodeCount: number
}

type BranchResult<T> =
  | { ok: true; value: T; state: ParserSnapshot }
  | { ok: false; error: string }

/* ── Modified UTF-8 ───────────────────────────────────── */

/**
 * Java writer 按 UTF-16 code unit 编码：NUL 是 C0 80，补充平面字符是两个
 * surrogate 各三字节；四字节 UTF-8 在这里反而非法。reader 端与 JDK 一致，
 * 也接受单字节 NUL 与 overlong 1/2/3-byte 组（writer 不会生成）。JS string 本身
 * 也是 UTF-16，所以逐 code unit 写入即可，并且能原样保留未配对 surrogate。
 */
function decodeModifiedUtf(raw: Uint8Array, fail: (message: string) => never): string {
  const chunks: string[] = []
  const units: number[] = []

  const flush = () => {
    if (units.length === 0) return
    chunks.push(String.fromCharCode(...units))
    units.length = 0
  }

  for (let i = 0; i < raw.length; i += 1) {
    const first = raw[i]
    let unit: number

    if (first <= 0x7f) {
      unit = first
    } else if ((first & 0xe0) === 0xc0) {
      if (i + 1 >= raw.length) fail('Modified UTF-8 的双字节字符没有写完')
      const second = raw[++i]
      if ((second & 0xc0) !== 0x80) fail('Modified UTF-8 的续字节格式不对')
      unit = ((first & 0x1f) << 6) | (second & 0x3f)
    } else if ((first & 0xf0) === 0xe0) {
      if (i + 2 >= raw.length) fail('Modified UTF-8 的三字节字符没有写完')
      const second = raw[++i]
      const third = raw[++i]
      if ((second & 0xc0) !== 0x80 || (third & 0xc0) !== 0x80) {
        fail('Modified UTF-8 的续字节格式不对')
      }
      unit = ((first & 0x0f) << 12) | ((second & 0x3f) << 6) | (third & 0x3f)
    } else {
      fail(`Modified UTF-8 不认识起始字节 0x${first.toString(16).padStart(2, '0')}`)
    }

    units.push(unit)
    // 避免把几百万个参数一次 spread 进 JS 调用栈。
    if (units.length >= 4096) flush()
  }

  flush()
  return chunks.join('')
}

/* ── 大端字节读取 ─────────────────────────────────────── */

class Reader {
  private readonly source: Uint8Array
  private readonly view: DataView
  private at = 0

  constructor(source: Uint8Array) {
    this.source = source
    this.view = new DataView(source.buffer, source.byteOffset, source.byteLength)
  }

  fail(message: string): never {
    throw new Error(`不是 Java 序列化数据（偏移 ${this.at}）：${message}`)
  }

  get done(): boolean {
    return this.at === this.source.length
  }

  get remaining(): number {
    return this.source.length - this.at
  }

  mark(): number {
    return this.at
  }

  restore(offset: number): void {
    if (!Number.isSafeInteger(offset) || offset < 0 || offset > this.source.length) {
      this.fail(`无法恢复到非法偏移 ${offset}`)
    }
    this.at = offset
  }

  peek(): number {
    this.need(1)
    return this.source[this.at]
  }

  private need(count: number): void {
    if (!Number.isSafeInteger(count) || count < 0) this.fail(`非法字节数 ${count}`)
    if (count > this.remaining) this.fail(`还差 ${count - this.remaining} 个字节`)
  }

  u8(): number {
    this.need(1)
    const value = this.view.getUint8(this.at)
    this.at += 1
    return value
  }

  i8(): number {
    this.need(1)
    const value = this.view.getInt8(this.at)
    this.at += 1
    return value
  }

  u16(): number {
    this.need(2)
    const value = this.view.getUint16(this.at)
    this.at += 2
    return value
  }

  i16(): number {
    this.need(2)
    const value = this.view.getInt16(this.at)
    this.at += 2
    return value
  }

  u32(): number {
    this.need(4)
    const value = this.view.getUint32(this.at)
    this.at += 4
    return value
  }

  i32(): number {
    this.need(4)
    const value = this.view.getInt32(this.at)
    this.at += 4
    return value
  }

  i64(): bigint {
    this.need(8)
    const value = this.view.getBigInt64(this.at)
    this.at += 8
    return value
  }

  f32(): number {
    this.need(4)
    const value = this.view.getFloat32(this.at)
    this.at += 4
    return value
  }

  f64(): number {
    this.need(8)
    const value = this.view.getFloat64(this.at)
    this.at += 8
    return value
  }

  /** 要挂进 Node 的字节一律复制，不能让小节点吊着整段 8 MB 输入。 */
  bytes(length: number): Uint8Array {
    this.need(length)
    const value = this.source.slice(this.at, this.at + length)
    this.at += length
    return value
  }

  modifiedUtf(length: number, what: string): string {
    const raw = this.bytes(length)
    return decodeModifiedUtf(raw, (message) => this.fail(`${what}：${message}`))
  }

  shortUtf(what: string): string {
    return this.modifiedUtf(this.u16(), what)
  }

  longUtf(what: string): string {
    const length = this.i64()
    if (length < 0n) this.fail(`${what}声明了负长度 ${length}`)
    if (length > BigInt(this.remaining)) {
      this.fail(`${what}声明了 ${length} 个字节，但只剩 ${this.remaining} 个`)
    }
    return this.modifiedUtf(Number(length), what)
  }

  checkCount(count: number, minBytes: number, what: string): void {
    if (!Number.isSafeInteger(count) || count < 0) this.fail(`${what}数量是非法值 ${count}`)
    if (count > MAX_ITEMS) this.fail(`${what}数量超过 ${MAX_ITEMS}`)
    if (count > Math.floor(this.remaining / minBytes)) {
      this.fail(`声明了 ${count} ${what}，剩下的字节装不下`)
    }
  }

  takePrimitiveArray(count: number, width: number, what: string): Uint8Array {
    if (!Number.isSafeInteger(count) || count < 0) this.fail(`${what}长度是非法值 ${count}`)
    if (count > Math.floor(this.remaining / width)) {
      this.fail(`${what}声明了 ${count} 项，剩下的字节装不下`)
    }
    return this.bytes(count * width)
  }
}

/* ── 协议解析 ─────────────────────────────────────────── */

class Parser {
  private readonly reader: Reader
  private readonly handles = new Map<number, HandleEntry>()
  private nextHandle = BASE_WIRE_HANDLE
  private epoch = 0
  private nodeCount = 0
  private backtrackHandleCopies = 0
  private backtrackBytes = 0
  private readingFatalException = false

  constructor(bytes: Uint8Array) {
    this.reader = new Reader(bytes)
  }

  private fail(message: string): never {
    return this.reader.fail(message)
  }

  private node<T extends Node>(value: T): T {
    this.nodeCount += 1
    if (this.nodeCount > MAX_NODES) this.fail(`节点数超过 ${MAX_NODES}`)
    return value
  }

  private assignHandle<T extends HandlePayload>(entry: T, origin = this.reader.mark()): T & { origin: number } {
    if (this.handles.size >= MAX_HANDLES) this.fail(`handle 数超过 ${MAX_HANDLES}`)
    const handle = this.nextHandle
    this.nextHandle += 1
    const stored = { ...entry, origin }
    this.handles.set(handle, stored)
    return stored
  }

  private resetHandles(): void {
    this.handles.clear()
    this.nextHandle = BASE_WIRE_HANDLE
    this.epoch += 1
  }

  private snapshot(): ParserSnapshot {
    this.backtrackHandleCopies += this.handles.size + 1
    if (this.backtrackHandleCopies > MAX_BACKTRACK_HANDLE_COPIES) {
      throw new BacktrackLimitError(
        `Java 序列化的 SC_WRITE_METHOD handle snapshot 超过 ${MAX_BACKTRACK_HANDLE_COPIES} 项`,
      )
    }
    return {
      offset: this.reader.mark(),
      handles: new Map(this.handles),
      nextHandle: this.nextHandle,
      epoch: this.epoch,
      nodeCount: this.nodeCount,
    }
  }

  private restore(snapshot: ParserSnapshot): void {
    this.reader.restore(snapshot.offset)
    this.handles.clear()
    for (const [handle, entry] of snapshot.handles) this.handles.set(handle, entry)
    this.nextHandle = snapshot.nextHandle
    this.epoch = snapshot.epoch
    this.nodeCount = snapshot.nodeCount
  }

  private branch<T>(work: () => T): BranchResult<T> {
    const start = this.reader.mark()
    try {
      const value = work()
      return { ok: true, value, state: this.snapshot() }
    } catch (error) {
      if (
        error instanceof WriteExceptionSignal ||
        error instanceof BacktrackLimitError ||
        error instanceof NestedWriteExceptionError
      ) {
        throw error
      }
      return { ok: false, error: error instanceof Error ? error.message : String(error) }
    } finally {
      this.backtrackBytes += Math.max(0, this.reader.mark() - start)
      if (this.backtrackBytes > MAX_BACKTRACK_BYTES) {
        throw new BacktrackLimitError(
          `Java 序列化的 SC_WRITE_METHOD 重复解析超过 ${MAX_BACKTRACK_BYTES} 字节`,
        )
      }
    }
  }

  private readHandle(): { handle: number; entry: HandleEntry } {
    const handle = this.reader.u32()
    const entry = this.handles.get(handle)
    if (!entry) this.fail(`handle 0x${handle.toString(16)} 不存在或已经被 reset`)
    return { handle, entry }
  }

  private referenceNode(handle: number, entry: HandleEntry): Node {
    return this.node({
      t: 'java',
      v: { kind: 'reference', epoch: this.epoch, handle, target: handleSummary(entry) },
    })
  }

  run(): Node {
    const magic = this.reader.u16()
    if (magic !== STREAM_MAGIC) {
      this.fail(`stream magic 应该是 ac ed，实际是 ${magic.toString(16).padStart(4, '0')}`)
    }
    const version = this.reader.u16()
    if (version !== STREAM_VERSION) {
      this.fail(`stream version 应该是 00 05，实际是 ${version.toString(16).padStart(4, '0')}`)
    }

    const contents: JavaContent[] = []
    while (!this.reader.done) {
      try {
        contents.push(this.readContent(0, true))
      } catch (error) {
        if (!(error instanceof WriteExceptionSignal)) throw error
        contents.push({ kind: 'value', value: error.value })
      }
    }

    // RedisTemplate 的常态是一条流只有一个根对象，去掉无信息量的 stream 包装；
    // 空流、多根对象、顶层 primitive blockdata 或 reset 则必须保留顺序。
    if (contents.length === 1 && contents[0].kind === 'value') return contents[0].value
    return this.node({ t: 'java', v: { kind: 'stream', contents } })
  }

  private readContent(depth: number, allowReset = false): JavaContent {
    if (depth > MAX_DEPTH) this.fail(`嵌套超过 ${MAX_DEPTH} 层`)
    const tag = this.reader.peek()

    if (tag === TC_BLOCKDATA || tag === TC_BLOCKDATALONG) {
      return { kind: 'block', value: this.readBlockData() }
    }
    if (tag === TC_RESET) {
      if (!allowReset) this.fail('TC_RESET 只能出现在顶层 content，不能嵌在对象或 annotation 里')
      this.reader.u8()
      this.resetHandles()
      return { kind: 'reset', epoch: this.epoch }
    }
    if (tag === TC_ENDBLOCKDATA) this.fail('TC_ENDBLOCKDATA 出现在没有 annotation 的地方')
    return { kind: 'value', value: this.readValue(depth) }
  }

  private readBlockData(): Uint8Array {
    const tag = this.reader.u8()
    if (tag === TC_BLOCKDATA) return this.reader.bytes(this.reader.u8())
    if (tag !== TC_BLOCKDATALONG) this.fail('这里应该是 blockdata')
    const length = this.reader.i32()
    if (length < 0) this.fail(`TC_BLOCKDATALONG 声明了负长度 ${length}`)
    return this.reader.bytes(length)
  }

  private readAnnotation(depth: number, what: string): JavaContent[] {
    const contents: JavaContent[] = []
    for (;;) {
      if (this.reader.done) this.fail(`${what}缺少 TC_ENDBLOCKDATA`)
      if (this.reader.peek() === TC_ENDBLOCKDATA) {
        this.reader.u8()
        return contents
      }
      if (contents.length >= MAX_ITEMS) this.fail(`${what}的内容项超过 ${MAX_ITEMS}`)
      contents.push(this.readContent(depth + 1))
    }
  }

  private readValue(depth: number): Node {
    if (depth > MAX_DEPTH) this.fail(`嵌套超过 ${MAX_DEPTH} 层`)
    const origin = this.reader.mark()
    const tag = this.reader.u8()

    switch (tag) {
      case TC_NULL:
        return this.node({ t: 'null' })
      case TC_REFERENCE: {
        const { handle, entry } = this.readHandle()
        return this.referenceNode(handle, entry)
      }
      case TC_CLASSDESC: {
        const descriptor = this.readNewClassDesc(depth + 1, false, origin)
        return this.node({ t: 'java', v: { kind: 'classDesc', classes: this.classChain(descriptor) } })
      }
      case TC_PROXYCLASSDESC: {
        const descriptor = this.readNewClassDesc(depth + 1, true, origin)
        return this.node({ t: 'java', v: { kind: 'classDesc', classes: this.classChain(descriptor) } })
      }
      case TC_OBJECT:
        return this.readNewObject(depth + 1, origin)
      case TC_STRING:
      case TC_LONGSTRING:
        return this.readNewString(tag, origin).node
      case TC_ARRAY:
        return this.readNewArray(depth + 1, origin)
      case TC_CLASS:
        return this.readNewClass(depth + 1, origin)
      case TC_ENUM:
        return this.readNewEnum(depth + 1, origin)
      case TC_EXCEPTION:
        return this.throwWriteException(depth + 1)
      case TC_RESET:
        this.fail('TC_RESET 只能出现在 content 边界，不能代替一个字段值')
      case TC_BLOCKDATA:
      case TC_BLOCKDATALONG:
        this.fail('blockdata 不是对象值，只能出现在 stream / annotation 内容里')
      case TC_ENDBLOCKDATA:
        this.fail('TC_ENDBLOCKDATA 不能当对象值')
      default:
        return this.fail(`不认识 type code 0x${tag.toString(16).padStart(2, '0')}`)
    }
  }

  /* ── class descriptor ──────────────────────────────── */

  private readClassDesc(depth: number): ClassDesc | null {
    if (depth > MAX_DEPTH) this.fail(`class descriptor 嵌套超过 ${MAX_DEPTH} 层`)
    const origin = this.reader.mark()
    const tag = this.reader.u8()
    if (tag === TC_NULL) return null
    if (tag === TC_REFERENCE) {
      const { handle, entry } = this.readHandle()
      if (entry.kind !== 'classDesc') {
        this.fail(`handle 0x${handle.toString(16)} 不是 class descriptor`)
      }
      if (!entry.value.complete) {
        this.fail(`handle 0x${handle.toString(16)} 指向尚未写完的 class descriptor`)
      }
      return entry.value
    }
    if (tag === TC_CLASSDESC) return this.readNewClassDesc(depth + 1, false, origin)
    if (tag === TC_PROXYCLASSDESC) return this.readNewClassDesc(depth + 1, true, origin)
    return this.fail(`class descriptor 位置出现了 0x${tag.toString(16).padStart(2, '0')}`)
  }

  private requiredClassDesc(depth: number, what: string): ClassDesc {
    const descriptor = this.readClassDesc(depth)
    if (!descriptor) this.fail(`${what}的 class descriptor 不能是 null`)
    return descriptor
  }

  private readNewClassDesc(depth: number, proxy: boolean, origin: number): ClassDesc {
    if (depth > MAX_DEPTH) this.fail(`class descriptor 嵌套超过 ${MAX_DEPTH} 层`)
    if (proxy) return this.readProxyClassDesc(depth, origin)

    const name = this.reader.shortUtf('类名')
    const serialVersionUid = this.reader.i64()
    const layer: JavaClassLayer = {
      kind: 'class',
      name,
      serialVersionUid,
      flags: null,
      interfaces: [],
      fields: [],
      annotations: [],
    }
    // 规范规定 handle 在 classDescInfo 之前分配；先挂占位 descriptor，annotation
    // 里的 back-reference 才能按正确编号解析，恶意的 superclass 自环也能被后验抓住。
    const descriptor: ClassDesc = { layer, superClass: null, complete: false }
    this.assignHandle({ kind: 'classDesc', value: descriptor }, origin)

    const flags = this.reader.u8()
    layer.flags = flags
    const fieldCount = this.reader.i16()
    if (fieldCount < 0) this.fail(`类 ${name} 声明了负字段数 ${fieldCount}`)
    this.reader.checkCount(fieldCount, 3, '字段')

    let objectFieldsStarted = false
    let previousPrimitive = ''
    let previousObject = ''
    const fieldNames = new Set<string>()

    for (let i = 0; i < fieldCount; i += 1) {
      const type = String.fromCharCode(this.reader.u8())
      if (!PRIMITIVE_TYPES.has(type) && !OBJECT_TYPES.has(type)) {
        this.fail(`字段 type code ${JSON.stringify(type)} 不合法`)
      }
      const fieldName = this.reader.shortUtf('字段名')
      if (fieldNames.has(fieldName)) this.fail(`类 ${name} 重复声明了字段 ${fieldName}`)
      fieldNames.add(fieldName)

      if (OBJECT_TYPES.has(type)) {
        objectFieldsStarted = true
        if (previousObject !== '' && fieldName <= previousObject) {
          this.fail(`类 ${name} 的对象字段没有按名称升序排列`)
        }
        previousObject = fieldName
      } else {
        if (objectFieldsStarted) this.fail(`类 ${name} 的 primitive 字段出现在对象字段之后`)
        if (previousPrimitive !== '' && fieldName <= previousPrimitive) {
          this.fail(`类 ${name} 的 primitive 字段没有按名称升序排列`)
        }
        previousPrimitive = fieldName
      }

      let className: string | null = null
      if (OBJECT_TYPES.has(type)) {
        className = this.readStringObject(depth + 1, '字段类型').value
        if (type === 'L' && !validFieldDescriptor(className, false)) {
          this.fail(`对象字段 ${fieldName} 的 descriptor ${JSON.stringify(className)} 不合法`)
        }
        if (type === '[' && !validFieldDescriptor(className, true)) {
          this.fail(`数组字段 ${fieldName} 的 descriptor ${JSON.stringify(className)} 不合法`)
        }
      }
      layer.fields.push({ type, name: fieldName, className })
    }

    this.validateFlags(layer)
    layer.annotations = this.readAnnotation(depth + 1, `类 ${name} 的 annotation`)
    descriptor.superClass = this.readClassDesc(depth + 1)
    descriptor.complete = true
    return descriptor
  }

  private readProxyClassDesc(depth: number, origin: number): ClassDesc {
    const layer: JavaClassLayer = {
      kind: 'proxy',
      name: '<dynamic proxy>',
      serialVersionUid: null,
      flags: null,
      interfaces: [],
      fields: [],
      annotations: [],
    }
    const descriptor: ClassDesc = { layer, superClass: null, complete: false }
    this.assignHandle({ kind: 'classDesc', value: descriptor }, origin)

    const count = this.reader.i32()
    if (count > 0xffff) this.fail(`proxy interface 数量 ${count} 超过 65535`)
    this.reader.checkCount(count, 2, 'proxy interface')
    for (let i = 0; i < count; i += 1) {
      layer.interfaces.push(this.reader.shortUtf('proxy interface 名'))
    }
    layer.annotations = this.readAnnotation(depth + 1, 'proxy class annotation')
    descriptor.superClass = this.readClassDesc(depth + 1)
    descriptor.complete = true
    return descriptor
  }

  private validateFlags(layer: JavaClassLayer): void {
    const flags = layer.flags as number
    if ((flags & ~KNOWN_FLAGS) !== 0) {
      this.fail(`类 ${layer.name} 含未知 classDescFlags 0x${(flags & ~KNOWN_FLAGS).toString(16)}`)
    }
    if (![0x00, 0x02, 0x03, 0x04, 0x0c, 0x12].includes(flags)) {
      this.fail(`类 ${layer.name} 的 classDescFlags 组合 0x${flags.toString(16)} 不合法`)
    }
    const serializable = (flags & SC_SERIALIZABLE) !== 0
    const externalizable = (flags & SC_EXTERNALIZABLE) !== 0
    if (serializable && externalizable) {
      this.fail(`类 ${layer.name} 同时标了 SC_SERIALIZABLE 与 SC_EXTERNALIZABLE`)
    }
    if ((flags & SC_WRITE_METHOD) !== 0 && !serializable) {
      this.fail(`类 ${layer.name} 的 SC_WRITE_METHOD 没有配 SC_SERIALIZABLE`)
    }
    if ((flags & SC_BLOCK_DATA) !== 0 && !externalizable) {
      this.fail(`类 ${layer.name} 的 SC_BLOCK_DATA 没有配 SC_EXTERNALIZABLE`)
    }
    if ((flags & SC_ENUM) !== 0) {
      if (!serializable || externalizable) this.fail(`枚举类 ${layer.name} 的 flags 组合不合法`)
      if (layer.fields.length !== 0) this.fail(`枚举类 ${layer.name} 不应该声明序列化字段`)
      if (layer.serialVersionUid !== 0n) this.fail(`枚举类 ${layer.name} 的 SUID 应该是 0`)
    }
    if (externalizable && layer.fields.length !== 0) {
      this.fail(`Externalizable 类 ${layer.name} 不应该有默认字段表`)
    }
    if (!serializable && !externalizable && layer.fields.length !== 0) {
      this.fail(`没有序列化 flags 的类 ${layer.name} 不应该有字段表`)
    }
  }

  private classChain(descriptor: ClassDesc): JavaClassLayer[] {
    const seen = new Set<ClassDesc>()
    const derivedFirst: JavaClassLayer[] = []
    let current: ClassDesc | null = descriptor

    while (current) {
      if (seen.has(current)) this.fail('class descriptor 的父类链形成了环')
      if (derivedFirst.length >= MAX_CLASS_CHAIN) {
        this.fail(`class descriptor 的父类链超过 ${MAX_CLASS_CHAIN} 层`)
      }
      seen.add(current)
      derivedFirst.push(current.layer)
      current = current.superClass
    }
    return derivedFirst.reverse()
  }

  private classNameOf(descriptor: ClassDesc): string {
    const layer = descriptor.layer
    if (layer.kind === 'class') return layer.name
    return `proxy(${layer.interfaces.join(', ')})`
  }

  /* ── strings / references ──────────────────────────── */

  private readNewString(tag: number, origin: number): StringObject {
    const entry = this.assignHandle({ kind: 'string', value: null as string | null }, origin)
    const value = tag === TC_STRING ? this.reader.shortUtf('String') : this.reader.longUtf('LongString')
    entry.value = value
    return { value, node: this.node({ t: 'str', v: value }) }
  }

  private readStringObject(depth: number, what: string): StringObject {
    if (depth > MAX_DEPTH) this.fail(`嵌套超过 ${MAX_DEPTH} 层`)
    const origin = this.reader.mark()
    const tag = this.reader.u8()
    if (tag === TC_STRING || tag === TC_LONGSTRING) return this.readNewString(tag, origin)
    if (tag === TC_REFERENCE) {
      const { handle, entry } = this.readHandle()
      if (entry.kind !== 'string' || entry.value === null) {
        this.fail(`${what}引用的 handle 0x${handle.toString(16)} 不是已经写完的 String`)
      }
      return { value: entry.value, node: this.referenceNode(handle, entry) }
    }
    return this.fail(`${what}应该是 String 或 String 引用，实际 type code 是 0x${tag.toString(16)}`)
  }

  /* ── objects / classdata ───────────────────────────── */

  private readNewObject(depth: number, origin: number): Node {
    const descriptor = this.requiredClassDesc(depth + 1, '对象')
    this.assignHandle({ kind: 'value', summary: `object ${this.classNameOf(descriptor)}` }, origin)
    const chain = this.classChain(descriptor)
    const classes: JavaClassData[] = chain.map((layer) => ({
      descriptor: layer,
      fields: [],
      custom: [],
      defaultFieldBoundary: null,
    }))

    const leaf = chain[chain.length - 1]
    if (
      leaf.kind === 'proxy' &&
      (descriptor.superClass?.layer.kind !== 'class' ||
        descriptor.superClass.layer.name !== 'java.lang.reflect.Proxy')
    ) {
      this.fail('dynamic proxy 的直接 superclass descriptor 必须是 java.lang.reflect.Proxy')
    }
    if (leaf.kind === 'class') {
      const leafFlags = leaf.flags as number
      if (leaf.name.startsWith('[')) this.fail(`数组类型 ${leaf.name} 应该用 TC_ARRAY，不能用 TC_OBJECT`)
      if ((leafFlags & (SC_SERIALIZABLE | SC_EXTERNALIZABLE)) === 0) {
        this.fail(`TC_OBJECT 的最派生类 ${leaf.name} 没有可用的序列化 flags`)
      }
    }
    if (leaf.kind === 'class' && ((leaf.flags as number) & SC_EXTERNALIZABLE) !== 0) {
      const flags = leaf.flags as number
      if ((flags & SC_BLOCK_DATA) === 0) {
        // Protocol 1 externalContents 没有结束标记；规范明确只有该类的
        // readExternal 知道该读多少。猜边界会把后面的对象吃错，必须拒绝。
        this.fail(`Externalizable 类 ${leaf.name} 使用 protocol 1，缺少可机械解析的结束边界`)
      }
      // Externalizable 的 writeExternal 负责**整个对象**，不会再沿 descriptor 链
      // 自动写父类字段；只有最派生类这一段 objectAnnotation。
      classes[classes.length - 1].custom = this.readAnnotation(
        depth + 1,
        `Externalizable 类 ${leaf.name} 的数据`,
      )
      return this.node({
        t: 'java',
        v: { kind: 'object', className: this.classNameOf(descriptor), classes },
      })
    }

    for (let index = 0; index < chain.length; index += 1) {
      const layer = chain[index]

      if (layer.kind === 'proxy') {
        continue
      }

      const flags = layer.flags as number
      if ((flags & SC_ENUM) !== 0) {
        this.fail(`枚举类 ${layer.name} 应该用 TC_ENUM，不能用 TC_OBJECT`)
      }

      if ((flags & SC_SERIALIZABLE) !== 0) {
        classes[index] = this.readSerializableClassData(layer, depth + 1)
      } else if ((flags & SC_EXTERNALIZABLE) !== 0) {
        this.fail(`Externalizable 类 ${layer.name} 不是最派生类，无法按普通 classdata 读取`)
      } else {
        this.fail(`TC_OBJECT 的 classdata 链包含没有序列化 flags 的类 ${layer.name}`)
      }
    }

    return this.node({
      t: 'java',
      v: { kind: 'object', className: this.classNameOf(descriptor), classes },
    })
  }

  /**
   * 有 `SC_WRITE_METHOD` 时，descriptor 仍列出默认字段，但类可以不调用
   * `defaultWriteObject()`；即使调用，也可以在它前后写 optional data。wire format
   * 没有一个 bit 或 marker 说明默认字段到底有没有出现、出现在第几个 content
   * 边界，所以对有字段的类必须枚举：
   *
   *   annotation-prefix + fields + annotation-suffix   或   只有 annotation
   *
   * 只有一个候选能完整走到 TC_ENDBLOCKDATA 才接受；多个候选就是格式本身对无
   * classpath 的查看器有二义性，拒绝比猜一个安全。HashMap 等正常流通常只有
   * 开头插字段那一条能走通，不调用 defaultWriteObject 的流走“只有 annotation”。
   */
  private readSerializableClassData(layer: JavaClassLayer, depth: number): JavaClassData {
    const readFields = () =>
      layer.fields.map((field) => ({
        descriptor: field,
        value: this.readFieldValue(field, depth + 1),
      }))

    if (((layer.flags as number) & SC_WRITE_METHOD) === 0 || layer.fields.length === 0) {
      return {
        descriptor: layer,
        fields: readFields(),
        custom:
          ((layer.flags as number) & SC_WRITE_METHOD) !== 0
            ? this.readAnnotation(depth + 1, `类 ${layer.name} 的 custom data`)
            : [],
        defaultFieldBoundary: 0,
      }
    }

    const start = this.snapshot()
    const candidates: Array<{ value: JavaClassData; state: ParserSnapshot }> = []
    let insertionError: string | null = null

    // 先试完全没有 defaultWriteObject 的形态。
    const omitted = this.branch(() => ({
      descriptor: layer,
      fields: [],
      custom: this.readAnnotation(depth + 1, `类 ${layer.name} 的 custom data`),
      defaultFieldBoundary: null,
    }))
    if (omitted.ok) candidates.push({ value: omitted.value, state: omitted.state })
    this.restore(start)

    // 再沿“把前面当 annotation content”这条路径逐个推进，每个边界试一次字段。
    const prefix: JavaContent[] = []
    for (let boundary = 0; boundary <= MAX_DEFAULT_FIELD_BOUNDARIES; boundary += 1) {
      const prefixState = this.snapshot()
      const insertion = this.branch(() => {
        const fields = readFields()
        const suffix = this.readAnnotation(depth + 1, `类 ${layer.name} 的 custom data`)
        return {
          descriptor: layer,
          fields,
          custom: [...prefix, ...suffix],
          defaultFieldBoundary: prefix.length,
        }
      })
      if (insertion.ok) {
        candidates.push({ value: insertion.value, state: insertion.state })
        if (candidates.length > MAX_AMBIGUOUS_LAYOUTS) {
          this.fail(`类 ${layer.name} 的默认字段布局超过 ${MAX_AMBIGUOUS_LAYOUTS} 种`)
        }
      } else if (insertionError === null) {
        insertionError = insertion.error
      }

      // insertion 分支不管成功失败都改了状态，回到当前 prefix 的边界。
      this.restore(prefixState)
      if (this.reader.done || this.reader.peek() === TC_ENDBLOCKDATA) break

      try {
        prefix.push(this.readContent(depth + 1))
      } catch (error) {
        if (
          error instanceof WriteExceptionSignal ||
          error instanceof BacktrackLimitError ||
          error instanceof NestedWriteExceptionError
        ) {
          throw error
        }
        break
      }

      if (boundary === MAX_DEFAULT_FIELD_BOUNDARIES) {
        this.fail(`类 ${layer.name} 的 defaultWriteObject 插入点超过 ${MAX_DEFAULT_FIELD_BOUNDARIES} 个`)
      }
    }

    this.restore(start)
    if (candidates.length === 0) {
      const detail = insertionError ?? (omitted.ok ? '' : omitted.error)
      this.fail(
        `类 ${layer.name} 的 SC_WRITE_METHOD 数据找不到唯一的默认字段边界${detail ? `；${detail}` : ''}`,
      )
    }
    if (candidates.length > 1) {
      const firstState = candidates[0].state
      if (!candidates.every((candidate) => equivalentState(firstState, candidate.state))) {
        this.fail(`类 ${layer.name} 的多个默认字段布局会留下不同的游标或 handle 状态，拒绝猜测`)
      }

      const retainedNodes =
        start.nodeCount +
        candidates.reduce(
          (total, candidate) => total + (candidate.state.nodeCount - start.nodeCount),
          0,
        )
      if (retainedNodes > MAX_NODES) this.fail(`节点数超过 ${MAX_NODES}`)

      this.restore(firstState)
      this.nodeCount = retainedNodes
      const primary = candidates[0].value
      primary.alternatives = candidates.map((candidate) => ({
        fields: candidate.value.fields,
        custom: candidate.value.custom,
        defaultFieldBoundary: candidate.value.defaultFieldBoundary,
      }))
      return primary
    }
    this.restore(candidates[0].state)
    return candidates[0].value
  }

  private readFieldValue(field: JavaFieldDescriptor, depth: number): Node {
    switch (field.type) {
      case 'B':
        return this.node({ t: 'int', v: BigInt(this.reader.i8()) })
      case 'C':
        return this.node({ t: 'str', v: String.fromCharCode(this.reader.u16()) })
      case 'D':
        return this.node({ t: 'float', v: this.reader.f64(), bits: 64 })
      case 'F':
        return this.node({ t: 'float', v: this.reader.f32(), bits: 32 })
      case 'I':
        return this.node({ t: 'int', v: BigInt(this.reader.i32()) })
      case 'J':
        return this.node({ t: 'int', v: this.reader.i64() })
      case 'S':
        return this.node({ t: 'int', v: BigInt(this.reader.i16()) })
      case 'Z':
        // DataInput.readBoolean 的语义是 0=false，其余=true；按接收端真实行为解析。
        return this.node({ t: 'bool', v: this.reader.u8() !== 0 })
      case 'L':
      case '[':
        return this.readValue(depth + 1)
      default:
        return this.fail(`字段 ${field.name} 的 type code ${field.type} 不认识`)
    }
  }

  /* ── arrays / enum / Class / exception ─────────────── */

  private readNewArray(depth: number, origin: number): Node {
    const descriptor = this.requiredClassDesc(depth + 1, '数组')
    this.assignHandle({ kind: 'value', summary: `array ${this.classNameOf(descriptor)}` }, origin)
    const className = this.classNameOf(descriptor)
    if (descriptor.layer.kind !== 'class' || descriptor.layer.flags !== SC_SERIALIZABLE) {
      this.fail(`TC_ARRAY 的 class descriptor flags 应该只有 SC_SERIALIZABLE`)
    }
    if (descriptor.layer.fields.length !== 0 || descriptor.superClass !== null) {
      this.fail('TC_ARRAY 的 class descriptor 不能有字段或 superclass')
    }
    if (!validArrayClassName(className)) {
      this.fail(`TC_ARRAY 的 class descriptor ${JSON.stringify(className)} 不是数组类型`)
    }

    const count = this.reader.i32()
    if (count < 0) this.fail(`数组 ${className} 声明了负长度 ${count}`)
    const component = className[1]
    const width = primitiveWidth(component)

    if (width !== null && className.length === 2) {
      const bytes = this.reader.takePrimitiveArray(count, width, `数组 ${className}`)
      return this.node({
        t: 'java',
        v: { kind: 'primitiveArray', className, component, count, bytes },
      })
    }

    if (component !== 'L' && component !== '[') {
      this.fail(`数组 ${className} 的 component descriptor 不合法`)
    }
    this.reader.checkCount(count, 1, '数组元素')
    const values: Node[] = []
    for (let i = 0; i < count; i += 1) values.push(this.readValue(depth + 1))
    return this.node({ t: 'java', v: { kind: 'array', className, values } })
  }

  private readNewEnum(depth: number, origin: number): Node {
    const descriptor = this.requiredClassDesc(depth + 1, '枚举')
    this.assignHandle({ kind: 'value', summary: `enum ${this.classNameOf(descriptor)}` }, origin)
    const classes = this.classChain(descriptor)
    const leaf = classes[classes.length - 1]
    if (leaf.kind !== 'class' || ((leaf.flags as number) & SC_ENUM) === 0) {
      this.fail(`TC_ENUM 的 class descriptor ${leaf.name} 没有 SC_ENUM`)
    }
    const name = this.readStringObject(depth + 1, '枚举常量名').node
    return this.node({
      t: 'java',
      v: { kind: 'enum', className: this.classNameOf(descriptor), name, classes },
    })
  }

  private readNewClass(depth: number, origin: number): Node {
    const descriptor = this.requiredClassDesc(depth + 1, 'Class')
    this.assignHandle({ kind: 'value', summary: `Class ${this.classNameOf(descriptor)}` }, origin)
    return this.node({ t: 'java', v: { kind: 'class', classes: this.classChain(descriptor) } })
  }

  private throwWriteException(depth: number): never {
    if (this.readingFatalException) {
      throw new NestedWriteExceptionError('Java 序列化的 TC_EXCEPTION 里面不能再嵌 TC_EXCEPTION')
    }
    this.readingFatalException = true
    let value: Node

    // TC_EXCEPTION 自身前后各清一次 handle 表，编号不能与损坏前后的图串起来。
    try {
      this.resetHandles()
      if (this.reader.peek() !== TC_OBJECT) {
        this.fail('TC_EXCEPTION 后面应该是一个 TC_OBJECT Throwable')
      }
      const origin = this.reader.mark()
      this.reader.u8()
      const throwable = this.readNewObject(depth + 1, origin)
      this.resetHandles()
      value = this.node({ t: 'java', v: { kind: 'exception', value: throwable } })
    } finally {
      this.readingFatalException = false
    }
    // fatal exception 表示当前 writeObject 中止；不能回到半截 enclosing object 继续读。
    throw new WriteExceptionSignal(value)
  }
}

class WriteExceptionSignal {
  readonly value: Node

  constructor(value: Node) {
    this.value = value
  }
}

class BacktrackLimitError extends Error {}

class NestedWriteExceptionError extends Error {}

function handleSummary(entry: HandleEntry): string {
  if (entry.kind === 'value') return entry.summary
  if (entry.kind === 'classDesc') {
    const layer = entry.value.layer
    return layer.kind === 'class'
      ? `classDesc ${layer.name}`
      : `classDesc proxy(${layer.interfaces.join(', ')})`
  }
  if (entry.value === null) return 'String (pending)'
  const shown = entry.value.length > 80 ? `${entry.value.slice(0, 80)}…` : entry.value
  return `String ${JSON.stringify(shown)}`
}

function equivalentState(left: ParserSnapshot, right: ParserSnapshot): boolean {
  if (
    left.offset !== right.offset ||
    left.nextHandle !== right.nextHandle ||
    left.epoch !== right.epoch ||
    left.handles.size !== right.handles.size
  ) {
    return false
  }

  for (const [handle, leftEntry] of left.handles) {
    const rightEntry = right.handles.get(handle)
    if (!rightEntry || handleEntrySignature(leftEntry) !== handleEntrySignature(rightEntry)) {
      return false
    }
  }
  return true
}

function handleEntrySignature(entry: HandleEntry): string {
  if (entry.kind === 'string') return `string@${entry.origin}:${JSON.stringify(entry.value)}`
  if (entry.kind === 'value') return `value@${entry.origin}:${entry.summary}`
  return `classDesc@${entry.origin}:${classDescSignature(entry.value, new Set())}`
}

function classDescSignature(descriptor: ClassDesc, seen: Set<ClassDesc>): string {
  if (seen.has(descriptor)) return '<cycle>'
  seen.add(descriptor)
  const layer = descriptor.layer
  const fields = layer.fields
    .map((field) => `${field.type}:${field.name}:${field.className ?? ''}`)
    .join(',')
  const own = [
    layer.kind,
    layer.name,
    String(layer.serialVersionUid),
    String(layer.flags),
    String(descriptor.complete),
    layer.interfaces.join(','),
    fields,
  ].join('|')
  const parent = descriptor.superClass
    ? classDescSignature(descriptor.superClass, seen)
    : '<null>'
  return `${own}->${parent}`
}

/** L/[ 字段使用 JVM field descriptor（类名以 `/` 分隔），不是 `Class.getName()`。 */
function validFieldDescriptor(value: string, requireArray: boolean): boolean {
  return validTypeDescriptor(value, requireArray, false)
}

/** TC_ARRAY 的 class name 来自 `Class.getName()`，对象数组的类名以 `.` 分隔。 */
function validArrayClassName(value: string): boolean {
  return validTypeDescriptor(value, true, true)
}

function validTypeDescriptor(value: string, requireArray: boolean, allowDot: boolean): boolean {
  let at = 0
  while (value[at] === '[') at += 1
  if (requireArray ? at === 0 : at !== 0) return false
  if (at >= value.length) return false

  const base = value[at]
  if (PRIMITIVE_TYPES.has(base)) return at === value.length - 1
  if (base !== 'L' || !value.endsWith(';') || at + 2 > value.length) return false

  const body = value.slice(at + 1, -1)
  if (body.length === 0) return false
  for (const char of body) {
    if (char === ';' || char === '[' || (!allowDot && char === '.')) return false
  }
  return true
}

function primitiveWidth(component: string): number | null {
  switch (component) {
    case 'B':
    case 'Z':
      return 1
    case 'C':
    case 'S':
      return 2
    case 'F':
    case 'I':
      return 4
    case 'D':
    case 'J':
      return 8
    default:
      return null
  }
}

function decode(bytes: Uint8Array): Node {
  return new Parser(bytes).run()
}

/** 自动识别只认完整的四字节 stream header；手点同样会由 parser 给出精确错误。 */
function detectPrefix(bytes: Uint8Array): boolean {
  return (
    bytes.length >= 4 &&
    bytes[0] === 0xac &&
    bytes[1] === 0xed &&
    bytes[2] === 0x00 &&
    bytes[3] === 0x05
  )
}

export const javaSerializeCodec: ReadonlyCodec = { decode, detectPrefix }
