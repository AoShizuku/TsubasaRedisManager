import type { RendererCodec } from './index'
import type { Node } from './types'

/**
 * MessagePack 的解析与重编码。
 *
 * ## 为什么自写，不用 `@msgpack/msgpack`
 *
 * 它的值模型是**惯用 JS 值**，而 `Node` 存在的全部理由正是惯用 JS 值装不下
 * msgpack：map 只能解成普通对象（键序与 int 键类型都丢）、`useBigInt64` 让每个
 * 小整数都写满 8 字节（于是每个含小整数的值都被往返自检判成只读）、float 宽度是
 * 全局开关、非 UTF-8 的 str 键拿不回原字节、`__proto__` 键被拒。逐条见
 * doc/frontend/codec-formats.md「不用 @msgpack/msgpack 的五条理由」。
 *
 * **依赖买的是「格式细节的正确性」**，而这里它只能卖给我们一次有损转换。所以
 * `@msgpack/msgpack` 是 devDependency，在 `msgpack.test.ts` 里当**独立实现的对照**
 * 用 —— 那是它真能提供价值的一层。
 *
 * ## 格式出处
 *
 * 独立编写，未参考也未移植任何现成实现（CLAUDE.md 的来源约束）。对照的是规范
 * 本身，不是谁的代码：
 *
 *   - msgpack/msgpack 的 `spec.md`，"Overview" 那张格式表：fixint / fixmap /
 *     fixarray / fixstr 的头字节区间，`0xc0`–`0xdf` 的分配，各族长度前缀的宽度，
 *     多字节整数一律**大端**，`0xc1` 标着 never used
 *   - 同一份 spec 的 "Timestamp extension type"：type = -1 只是 ext 的一个取值，
 *     所以这里不特判，原样进 `Node.ext`（`types.ts` 的 `Node` 就是这么规定的）
 *   - str 与 bin 是两族：str 规定是 UTF-8，bin 是任意字节。这是 spec 区分它们的
 *     **唯一**理由，也是下面 `readString()` 那段的全部依据
 *
 * ## 严格是刚性要求
 *
 * 解完必须消耗掉全部输入，有剩余就抛错；**decode 抛错 = 不是这个格式**，不要
 * 返回"部分解出来"的结构。msgpack 自己进不了自动识别的候选集（`lib/codecs.ts`
 * 里 `detectable: false`，理由见 doc/frontend/codecs.md「识别的三个维度」：
 * 任意字节都可能是合法 msgpack），但
 * `checkRoundTrip` 与将来的连接级配置都按"解不开就抛"来用它。
 */

/* ── 格式标记。数值出自 spec.md 的 Overview 表 ─────────── */

const POSITIVE_FIXINT_MAX = 0x7f
const FIXMAP_BASE = 0x80
const FIXARRAY_BASE = 0x90
const FIXSTR_BASE = 0xa0
const NIL = 0xc0
const NEVER_USED = 0xc1
const FALSE = 0xc2
const TRUE = 0xc3
const BIN8 = 0xc4
const BIN16 = 0xc5
const BIN32 = 0xc6
const EXT8 = 0xc7
const EXT16 = 0xc8
const EXT32 = 0xc9
const FLOAT32 = 0xca
const FLOAT64 = 0xcb
const UINT8 = 0xcc
const UINT16 = 0xcd
const UINT32 = 0xce
const UINT64 = 0xcf
const INT8 = 0xd0
const INT16 = 0xd1
const INT32 = 0xd2
const INT64 = 0xd3
const FIXEXT1 = 0xd4
const FIXEXT2 = 0xd5
const FIXEXT4 = 0xd6
const FIXEXT8 = 0xd7
const FIXEXT16 = 0xd8
const STR8 = 0xd9
const STR16 = 0xda
const STR32 = 0xdb
const ARRAY16 = 0xdc
const ARRAY32 = 0xdd
const MAP16 = 0xde
const MAP32 = 0xdf
const NEGATIVE_FIXINT_BASE = 0xe0

/** fixstr / fixarray / fixmap 各自能装的最大项数（头字节低四位或低五位）。 */
const FIXSTR_MAX = 0x1f
const FIXCONTAINER_MAX = 0x0f

/**
 * `Node.int` 一格要同时装下 msgpack 的 int64 与 uint64，所以范围是两族的并集：
 * `[-2^63, 2^64-1]`。这不是 int64 —— uint64 的上半段超出 int64 的正端。
 */
const INT64_MIN = -9223372036854775808n
const UINT64_MAX = 18446744073709551615n

/** 长度前缀最宽是 uint32，所以四类容器的项数 / 字节数都到此为止。 */
const U32_MAX = 0xffffffff

/**
 * 嵌套上限，纯防御：`91 91 91 …` 这样的输入能让递归下降炸栈，而炸栈抛的是
 * `RangeError`，混在"解不开"里看不出到底出了什么事。跟 `php.ts` 取同一个值。
 */
const MAX_DEPTH = 256

/**
 * `fatal: true` 是这个文件的要害：str 族**规定**是 UTF-8，但实践中有库往里塞
 * 任意字节。默认的宽松解码会把它们换成一串 U+FFFD —— 那是篡改数据，而且不可逆。
 * 解不开就说明"这段不是文本"，该进 `bytes`。
 *
 * `ignoreBOM: true` 同理：默认会**吃掉**开头的 U+FEFF，解出来的串比原字节少三个。
 */
const UTF8_DECODER = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true })
const UTF8_ENCODER = new TextEncoder()

/* ── 解析 ─────────────────────────────────────────────── */

class Reader {
  private readonly src: Uint8Array
  private readonly view: DataView
  private at = 0

  constructor(src: Uint8Array) {
    this.src = src
    // src 可能本身就是别人 buffer 上的一段视图，偏移与长度都要带上
    this.view = new DataView(src.buffer, src.byteOffset, src.byteLength)
  }

  /** 错误信息会直接显示给用户，所以要说清"在哪里""期待什么"。 */
  fail(message: string): never {
    throw new Error(`不是 MessagePack 数据（偏移 ${this.at}）：${message}`)
  }

  private need(count: number): void {
    if (this.at + count > this.src.length) {
      this.fail(`还差 ${this.at + count - this.src.length} 个字节`)
    }
  }

  /** 解完必须一个字节都不剩，见文件头"严格是刚性要求"。 */
  expectEnd(): void {
    if (this.at !== this.src.length) {
      this.fail(`末尾还剩 ${this.src.length - this.at} 个字节没有解释`)
    }
  }

  /* 多字节整数一律大端，所以 DataView 的 littleEndian 参数全部留默认的 false */

  private u8(): number {
    this.need(1)
    const value = this.view.getUint8(this.at)
    this.at += 1
    return value
  }

  private i8(): number {
    this.need(1)
    const value = this.view.getInt8(this.at)
    this.at += 1
    return value
  }

  private u16(): number {
    this.need(2)
    const value = this.view.getUint16(this.at)
    this.at += 2
    return value
  }

  private u32(): number {
    this.need(4)
    const value = this.view.getUint32(this.at)
    this.at += 4
    return value
  }

  /** 取 `len` 个字节的**视图**。要留住的地方自己 `slice`，见 `takeBytes`。 */
  private takeView(len: number): Uint8Array {
    this.need(len)
    const out = this.src.subarray(this.at, this.at + len)
    this.at += len
    return out
  }

  /**
   * 取 `len` 个字节的**副本**。`subarray` 会让节点吊着整段输入的引用 ——
   * 这里的输入可以有 8 MB（`FULL_VALUE_LIMIT`），一个两字节的 bin 不该把它钉住。
   */
  private takeBytes(len: number): Uint8Array {
    return this.takeView(len).slice()
  }

  /**
   * 容器的项数守卫。**不是洁癖，是拒绝服务防护**：`dd ff ff ff ff` 声明四十亿项，
   * 没有这一卡就会先去转四十亿次循环。每项至少占一个字节（map 的每对至少两个），
   * 所以拿剩余字节数当上界既精确又不花钱。
   */
  private checkCount(count: number, bytesPerItem: number, what: string): void {
    if (count * bytesPerItem > this.src.length - this.at) {
      this.fail(`声明了 ${count} ${what}，剩下的字节装不下`)
    }
  }

  /**
   * str 族：确认是合法 UTF-8 才当文本，否则原样进 `bytes`。
   *
   * 代价要写明白：`Node` 分不出"本来是 str 的 bytes"与"本来就是 bin 的 bytes"，
   * 所以这种值重编时会写成 bin，字节对不上，往返自检把它判成只读。
   * **这正是 `index.ts` 的 `checkRoundTrip` 点名的"msgpack 的 str/bin 歧义"**，
   * 是设计好的结局而不是漏洞：宁可只读，也不能把 U+FFFD 写回 Redis。
   */
  private readString(len: number): Node {
    const raw = this.takeView(len)
    try {
      return { t: 'str', v: UTF8_DECODER.decode(raw) }
    } catch {
      return { t: 'bytes', v: raw.slice() }
    }
  }

  private readArray(count: number, depth: number): Node {
    this.checkCount(count, 1, '项')
    const items: Node[] = []
    for (let i = 0; i < count; i += 1) items.push(this.readValue(depth + 1))
    return { t: 'arr', v: items }
  }

  /**
   * map 的键可以是任意类型，而且**有序** —— 所以是 `Array<[Node, Node]>`
   * 而不是 `Record`，见 types.ts。这里不去查重也不去排序：重复键与非常规键序
   * 都是输入的事实，归一了就再也编不回原字节。
   */
  private readMap(count: number, depth: number): Node {
    this.checkCount(count, 2, '对键值')
    const pairs: Array<[Node, Node]> = []
    for (let i = 0; i < count; i += 1) {
      const key = this.readValue(depth + 1)
      pairs.push([key, this.readValue(depth + 1)])
    }
    return { t: 'map', v: pairs }
  }

  /** ext 的排布是 `[头][长度?][type: int8][data]`，type 是**有符号**的一字节。 */
  private readExt(len: number): Node {
    const code = this.i8()
    return { t: 'ext', code, v: this.takeBytes(len) }
  }

  readValue(depth: number): Node {
    if (depth > MAX_DEPTH) this.fail(`嵌套超过 ${MAX_DEPTH} 层`)
    const tag = this.u8()

    // 三段带内联长度的头字节先挑出来，剩下的才是 0xc0–0xdf 那张表
    if (tag <= POSITIVE_FIXINT_MAX) return { t: 'int', v: BigInt(tag) }
    if (tag >= NEGATIVE_FIXINT_BASE) return { t: 'int', v: BigInt(tag - 0x100) }
    if (tag < FIXARRAY_BASE) return this.readMap(tag - FIXMAP_BASE, depth)
    if (tag < FIXSTR_BASE) return this.readArray(tag - FIXARRAY_BASE, depth)
    if (tag < NIL) return this.readString(tag - FIXSTR_BASE)

    switch (tag) {
      case NIL:
        return { t: 'null' }
      // spec 把 0xc1 标成 never used。放行它等于承认一种没有含义的字节
      case NEVER_USED:
        this.at -= 1
        return this.fail('0xc1 在 spec 里标着 never used')
      case FALSE:
        return { t: 'bool', v: false }
      case TRUE:
        return { t: 'bool', v: true }

      case BIN8:
        return { t: 'bytes', v: this.takeBytes(this.u8()) }
      case BIN16:
        return { t: 'bytes', v: this.takeBytes(this.u16()) }
      case BIN32:
        return { t: 'bytes', v: this.takeBytes(this.u32()) }

      case EXT8:
        return this.readExt(this.u8())
      case EXT16:
        return this.readExt(this.u16())
      case EXT32:
        return this.readExt(this.u32())

      // float32 与 float64 是两种编码，宽度要如实填。**不填 `literal`** ——
      // 那个字段是给文本格式的（同一个值多种写法），二进制定宽 float 表示唯一
      case FLOAT32: {
        this.need(4)
        const value = this.view.getFloat32(this.at)
        this.at += 4
        return { t: 'float', v: value, bits: 32 }
      }
      case FLOAT64: {
        this.need(8)
        const value = this.view.getFloat64(this.at)
        this.at += 8
        return { t: 'float', v: value, bits: 64 }
      }

      // 一律进 bigint：uint64 / int64 塞进 number 会静默丢精度，而 `Node.int`
      // 只有一种形态，小整数走 number 大整数走 bigint 只会让编码侧无所适从
      case UINT8:
        return { t: 'int', v: BigInt(this.u8()) }
      case UINT16:
        return { t: 'int', v: BigInt(this.u16()) }
      case UINT32:
        return { t: 'int', v: BigInt(this.u32()) }
      case UINT64: {
        this.need(8)
        const value = this.view.getBigUint64(this.at)
        this.at += 8
        return { t: 'int', v: value }
      }
      case INT8:
        return { t: 'int', v: BigInt(this.i8()) }
      case INT16: {
        this.need(2)
        const value = this.view.getInt16(this.at)
        this.at += 2
        return { t: 'int', v: BigInt(value) }
      }
      case INT32: {
        this.need(4)
        const value = this.view.getInt32(this.at)
        this.at += 4
        return { t: 'int', v: BigInt(value) }
      }
      case INT64: {
        this.need(8)
        const value = this.view.getBigInt64(this.at)
        this.at += 8
        return { t: 'int', v: value }
      }

      // fixext 的长度写死在头字节里，timestamp（type = -1）走的也是这几条
      case FIXEXT1:
        return this.readExt(1)
      case FIXEXT2:
        return this.readExt(2)
      case FIXEXT4:
        return this.readExt(4)
      case FIXEXT8:
        return this.readExt(8)
      case FIXEXT16:
        return this.readExt(16)

      case STR8:
        return this.readString(this.u8())
      case STR16:
        return this.readString(this.u16())
      case STR32:
        return this.readString(this.u32())

      case ARRAY16:
        return this.readArray(this.u16(), depth)
      case ARRAY32:
        return this.readArray(this.u32(), depth)

      case MAP16:
        return this.readMap(this.u16(), depth)
      case MAP32:
        return this.readMap(this.u32(), depth)

      default:
        // 0xc0–0xdf 每一格 spec 都分配了，走到这里说明上面漏了一条
        this.at -= 1
        this.fail(`没有 0x${tag.toString(16)} 这种头字节`)
    }
  }
}

function decode(bytes: Uint8Array): Node {
  const reader = new Reader(bytes)
  const node = reader.readValue(0)
  reader.expectEnd()
  return node
}

/* ── 重编码 ───────────────────────────────────────────── */

/**
 * 一律写**最短形式**：`1` 写成 fixint `0x01` 而不是 uint8 `0xcc 0x01`。
 *
 * spec 允许同一个值用不同宽度编码，但主流写库都只写最短的（`@msgpack/msgpack`
 * 的 `encodeNumber` / `writeStringHeader` 就是这么分档的，`msgpack.test.ts` 拿它
 * 交叉验证过）。所以非最短编码的输入重编后字节对不上 → 往返自检判只读。
 *
 * **这是正确行为，不要为它开特例。** 跟 PHP 的 float 字面量不同：那里是标准
 * PHP 自己就产出多种写法，属于常态，所以 `Node.float.literal` 把选择记了下来；
 * msgpack 的非最短编码是罕见的非标准输出，为它留后门等于放弃自检。
 */
class Writer {
  private buf = new Uint8Array(256)
  private view = new DataView(this.buf.buffer)
  private len = 0

  private need(extra: number): void {
    if (this.len + extra <= this.buf.length) return
    let size = this.buf.length * 2
    while (size < this.len + extra) size *= 2
    const grown = new Uint8Array(size)
    grown.set(this.buf.subarray(0, this.len))
    this.buf = grown
    this.view = new DataView(grown.buffer)
  }

  u8(value: number): void {
    this.need(1)
    this.view.setUint8(this.len, value)
    this.len += 1
  }

  i8(value: number): void {
    this.need(1)
    this.view.setInt8(this.len, value)
    this.len += 1
  }

  u16(value: number): void {
    this.need(2)
    this.view.setUint16(this.len, value)
    this.len += 2
  }

  i16(value: number): void {
    this.need(2)
    this.view.setInt16(this.len, value)
    this.len += 2
  }

  u32(value: number): void {
    this.need(4)
    this.view.setUint32(this.len, value)
    this.len += 4
  }

  i32(value: number): void {
    this.need(4)
    this.view.setInt32(this.len, value)
    this.len += 4
  }

  u64(value: bigint): void {
    this.need(8)
    this.view.setBigUint64(this.len, value)
    this.len += 8
  }

  i64(value: bigint): void {
    this.need(8)
    this.view.setBigInt64(this.len, value)
    this.len += 8
  }

  f32(value: number): void {
    this.need(4)
    this.view.setFloat32(this.len, value)
    this.len += 4
  }

  f64(value: number): void {
    this.need(8)
    this.view.setFloat64(this.len, value)
    this.len += 8
  }

  bytes(chunk: Uint8Array): void {
    this.need(chunk.length)
    this.buf.set(chunk, this.len)
    this.len += chunk.length
  }

  take(): Uint8Array {
    return this.buf.slice(0, this.len)
  }
}

/** 非负走 uint 族、负数走 int 族，各自取装得下的最窄一档。 */
function writeInt(writer: Writer, value: bigint): void {
  if (value < INT64_MIN || value > UINT64_MAX) {
    throw new Error('整数超出 MessagePack 的 int64 / uint64 范围')
  }
  if (value >= 0n) {
    if (value <= BigInt(POSITIVE_FIXINT_MAX)) return writer.u8(Number(value))
    if (value < 0x100n) {
      writer.u8(UINT8)
      return writer.u8(Number(value))
    }
    if (value < 0x10000n) {
      writer.u8(UINT16)
      return writer.u16(Number(value))
    }
    if (value < 0x100000000n) {
      writer.u8(UINT32)
      return writer.u32(Number(value))
    }
    writer.u8(UINT64)
    return writer.u64(value)
  }

  if (value >= -0x20n) return writer.u8(NEGATIVE_FIXINT_BASE | (Number(value) + 0x20))
  if (value >= -0x80n) {
    writer.u8(INT8)
    return writer.i8(Number(value))
  }
  if (value >= -0x8000n) {
    writer.u8(INT16)
    return writer.i16(Number(value))
  }
  if (value >= -0x80000000n) {
    writer.u8(INT32)
    return writer.i32(Number(value))
  }
  writer.u8(INT64)
  return writer.i64(value)
}

/**
 * `bits` 说 32 就写 32，但**先确认这个值在 float32 里存得下**。
 *
 * 从输入解出来的 f32 天然满足（`Math.fround` 是幂等的），所以这一卡只会打到
 * 用户**改过**的值：把 `0.1` 填进一个 f32 节点会被 `setFloat32` 悄悄舍成
 * `0.10000000149…`，那是静默改值。宁可在这里抛错说清楚。
 *
 * `Object.is` 而不是 `===`：`NaN` 与 `-0` 都要放行（前者 `===` 判不了，
 * 后者是 msgpack 里一串真实存在的、与 `+0` 不同的字节）。
 */
function writeFloat(writer: Writer, value: number, bits: 32 | 64): void {
  if (bits === 64) {
    writer.u8(FLOAT64)
    return writer.f64(value)
  }
  if (!Object.is(Math.fround(value), value)) {
    throw new Error(`${value} 在 float32 里存不下，改成 float64 才不会丢精度`)
  }
  writer.u8(FLOAT32)
  return writer.f32(value)
}

function writeStringHeader(writer: Writer, len: number): void {
  if (len <= FIXSTR_MAX) return writer.u8(FIXSTR_BASE + len)
  if (len < 0x100) {
    writer.u8(STR8)
    return writer.u8(len)
  }
  if (len < 0x10000) {
    writer.u8(STR16)
    return writer.u16(len)
  }
  if (len > U32_MAX) throw new Error(`字符串太长：${len} 字节`)
  writer.u8(STR32)
  return writer.u32(len)
}

function writeBinaryHeader(writer: Writer, len: number): void {
  // bin 没有 fix 档，最短就是 bin8
  if (len < 0x100) {
    writer.u8(BIN8)
    return writer.u8(len)
  }
  if (len < 0x10000) {
    writer.u8(BIN16)
    return writer.u16(len)
  }
  if (len > U32_MAX) throw new Error(`二进制太长：${len} 字节`)
  writer.u8(BIN32)
  return writer.u32(len)
}

function writeContainerHeader(
  writer: Writer,
  count: number,
  fixBase: number,
  tag16: number,
  tag32: number,
  what: string,
): void {
  if (count <= FIXCONTAINER_MAX) return writer.u8(fixBase + count)
  if (count < 0x10000) {
    writer.u8(tag16)
    return writer.u16(count)
  }
  if (count > U32_MAX) throw new Error(`${what}太长：${count} 项`)
  writer.u8(tag32)
  return writer.u32(count)
}

/** fixext 只覆盖 1 / 2 / 4 / 8 / 16 这五个长度，其余落到 ext8 / 16 / 32。 */
function writeExtHeader(writer: Writer, code: number, len: number): void {
  if (!Number.isInteger(code) || code < -128 || code > 127) {
    throw new Error(`ext 的 type 是一个有符号字节，${code} 装不进去`)
  }
  if (len === 1) writer.u8(FIXEXT1)
  else if (len === 2) writer.u8(FIXEXT2)
  else if (len === 4) writer.u8(FIXEXT4)
  else if (len === 8) writer.u8(FIXEXT8)
  else if (len === 16) writer.u8(FIXEXT16)
  else if (len < 0x100) {
    writer.u8(EXT8)
    writer.u8(len)
  } else if (len < 0x10000) {
    writer.u8(EXT16)
    writer.u16(len)
  } else if (len > U32_MAX) {
    throw new Error(`ext 太长：${len} 字节`)
  } else {
    writer.u8(EXT32)
    writer.u32(len)
  }
  writer.i8(code)
}

function writeValue(writer: Writer, node: Node, depth: number): void {
  if (depth > MAX_DEPTH) throw new Error(`嵌套超过 ${MAX_DEPTH} 层`)

  switch (node.t) {
    case 'null':
      return writer.u8(NIL)
    case 'bool':
      return writer.u8(node.v ? TRUE : FALSE)
    case 'int':
      return writeInt(writer, node.v)
    case 'float':
      // `literal` 是文本格式的东西，这里既不读也不写，见 types.ts 那一节
      return writeFloat(writer, node.v, node.bits)
    case 'str': {
      const raw = UTF8_ENCODER.encode(node.v)
      writeStringHeader(writer, raw.length)
      return writer.bytes(raw)
    }
    case 'bytes':
      writeBinaryHeader(writer, node.v.length)
      return writer.bytes(node.v)
    case 'arr': {
      writeContainerHeader(writer, node.v.length, FIXARRAY_BASE, ARRAY16, ARRAY32, '数组')
      for (const item of node.v) writeValue(writer, item, depth + 1)
      return
    }
    case 'map': {
      writeContainerHeader(writer, node.v.length, FIXMAP_BASE, MAP16, MAP32, 'map')
      for (const [key, value] of node.v) {
        writeValue(writer, key, depth + 1)
        writeValue(writer, value, depth + 1)
      }
      return
    }
    case 'ext':
      writeExtHeader(writer, node.code, node.v.length)
      return writer.bytes(node.v)
    case 'obj':
    case 'field':
      throw new Error('MessagePack 里没有 PHP 的类 / protobuf 字段这种东西')
    case 'seq':
    case 'call':
      // pickle 的 tuple / 构造调用。msgpack 的数组装得下前者的**元素**，但装不下
      // "这是个 tuple 不是 list"，而后者根本没有对应物 —— 硬编一个等于篡改类型
      throw new Error('MessagePack 里没有 tuple / 构造调用这种东西')
    case 'java':
      throw new Error('MessagePack 里没有 Java class / handle / blockdata 这种东西')
  }
}

function encode(node: Node): Uint8Array {
  const writer = new Writer()
  writeValue(writer, node, 0)
  return writer.take()
}

/**
 * 注册进 `RENDERER_CODECS.msgpack` 的那一项，挂载写在 `index.ts` 的表里。
 *
 * 注意它进不了自动识别的候选集（`lib/codecs.ts` 的 `detectable: false`）：
 * 任意字节都可能是合法 msgpack，进了候选就会把一切都识别成它。
 * "能不能跑"与"能不能自动识别"是两个维度，见 doc/frontend/codecs.md「识别的三个维度」。
 */
export const msgpackCodec: RendererCodec = { decode, encode }
