import type { ReadonlyCodec } from './index'
import type { Node } from './types'

/**
 * Python `pickle` 的**只读**解析（protocol 0–5）。
 *
 * ## 只解析，绝不实例化
 *
 * pickle 是一台**栈式虚拟机**，`GLOBAL` 取到一个可调用对象、`REDUCE` 调用它 ——
 * 这就是"反序列化即执行"的字面含义，一个构造过的 payload 能在 `pickle.loads()`
 * 那一刻拿到代码执行。本模块**只解释字节布局**：`GLOBAL` 解出模块名与类名，
 * `REDUCE` 解出"会调用谁、拿什么参数"，然后把它们摆进 `Node.call` 给用户看。
 * 没有任何一处会去解析那个名字、加载模块、或调用什么。
 *
 * 这样做**降低**用户的风险而不是增加：想看一个 pickle 值，替代方案是自己去跑
 * `pickle.loads()`，那才是真中招。解出来的 `call` 节点顺带让本工具成为一个
 * payload 检查器 —— 一个 `os.system` 摆在屏幕上，比藏起来安全得多。
 *
 * **没有 `encode`，而且这不是待补的缺口**，见 doc/backend/codec.md「Readonly 安全边界」：
 * 写回一个 pickle 值等于制造一份会被别的程序加载并执行的载荷，风险落在下游、
 * 我们既控制不了也看不见。所以它走 `ReadonlyCodec` 而不是 `RendererCodec`。
 *
 * ## 格式出处
 *
 * 独立编写，未参考也未移植任何现成实现（CLAUDE.md 的来源约束）。对照的是
 * 规范文档与 CPython 的**可观察行为**，不是它的代码：
 *
 *   - Python 标准库 `pickle` 模块文档，以及 protocol 的三份提案：
 *     PEP 307（protocol 2）、PEP 3154（protocol 4）、PEP 574（protocol 5，
 *     out-of-band buffer）
 *   - `Lib/pickletools.py` 的 opcode 表 —— 那是一份**格式文档**，每个 opcode
 *     的 `doc` 写明了参数的字节布局与栈效果
 *   - 各 opcode 的栈语义以 CPython 的可观察行为为准（喂进去什么字节、
 *     `pickletools.dis` 出来什么），逐条见下面的注释
 *
 * ## 没有往返自检，所以严格是唯一的机械保证
 *
 * 另外三个解析器靠"解完立刻重编、逐字节比对"给自己兜底（往返自检），只读类没有
 * 这一层。替代物是**消费完整性**（doc/frontend/codec-formats.md「消费完整性」），
 * 两条都在 `decode()` 末尾断言：
 *
 *   1. 必须读到 `STOP`，且**其后一个字节都不剩**
 *   2. `STOP` 时栈上**恰好一个值**
 *
 * 第 2 条比第 1 条强得多：它能抓住"某个 opcode 的参数个数数错了"这类 bug ——
 * 那种错误不会让读取越界，只会让栈上多一个少一个。
 *
 * **注意第 1 条与 CPython 有意不同**：`pickle.loads()` 读到 `STOP` 就返回，
 * 尾部的垃圾字节它不管。这里拒绝，理由和 `php.ts` 一样 —— `detectCodec` 拿
 * "decode 抛不抛错"当自动识别的判据，宽松的解析器会把任何以 `.` 结尾的垃圾
 * 也认成 pickle。
 *
 * ## 恶意输入：风险从 RCE 降级为资源耗尽，但没有归零
 *
 * 不实例化挡掉了代码执行，剩下的是内存与时间（doc/frontend/codec-formats.md「资源耗尽」）。
 * 逐条对策：
 *
 *   - **绝不按声明的长度预分配** —— `BINBYTES` 能声明 4 GB。一律先验剩余字节够不够
 *   - `FRAME` 的长度不能越过输入末尾
 *   - memo 取不到就抛错，不给默认值（一个 `undefined` 混进树里比失败难查得多）
 *   - 节点总数、`MARK` 层数、嵌套深度各有上限
 */

/* ── opcode。值就是那个 ASCII 字符或字节，注释里标出来便于对读 hexdump ── */

/* protocol 0：参数是文本，以 \n 结尾 */
const OP_MARK = 0x28 // (
const OP_STOP = 0x2e // .
const OP_POP = 0x30 // 0
const OP_POP_MARK = 0x31 // 1
const OP_DUP = 0x32 // 2
const OP_FLOAT = 0x46 // F
const OP_INT = 0x49 // I
const OP_LONG = 0x4c // L
const OP_NONE = 0x4e // N
const OP_PERSID = 0x50 // P
const OP_REDUCE = 0x52 // R
const OP_STRING = 0x53 // S
const OP_UNICODE = 0x56 // V
const OP_APPEND = 0x61 // a
const OP_BUILD = 0x62 // b
const OP_GLOBAL = 0x63 // c
const OP_DICT = 0x64 // d
const OP_GET = 0x67 // g
const OP_INST = 0x69 // i
const OP_LIST = 0x6c // l
const OP_OBJ = 0x6f // o
const OP_PUT = 0x70 // p
const OP_SETITEM = 0x73 // s
const OP_TUPLE = 0x74 // t

/* protocol 1：二进制参数 */
const OP_EMPTY_TUPLE = 0x29 // )
const OP_BINFLOAT = 0x47 // G
const OP_BININT = 0x4a // J
const OP_BININT1 = 0x4b // K
const OP_BININT2 = 0x4d // M
const OP_BINPERSID = 0x51 // Q
const OP_BINSTRING = 0x54 // T
const OP_SHORT_BINSTRING = 0x55 // U
const OP_BINUNICODE = 0x58 // X
const OP_EMPTY_LIST = 0x5d // ]
const OP_APPENDS = 0x65 // e
const OP_BINGET = 0x68 // h
const OP_LONG_BINGET = 0x6a // j
const OP_BINPUT = 0x71 // q
const OP_LONG_BINPUT = 0x72 // r
const OP_SETITEMS = 0x75 // u
const OP_EMPTY_DICT = 0x7d // }

/* protocol 2 */
const OP_PROTO = 0x80
const OP_NEWOBJ = 0x81
const OP_EXT1 = 0x82
const OP_EXT2 = 0x83
const OP_EXT4 = 0x84
const OP_TUPLE1 = 0x85
const OP_TUPLE2 = 0x86
const OP_TUPLE3 = 0x87
const OP_NEWTRUE = 0x88
const OP_NEWFALSE = 0x89
const OP_LONG1 = 0x8a
const OP_LONG4 = 0x8b

/* protocol 3：Python 3 的 bytes */
const OP_BINBYTES = 0x42 // B
const OP_SHORT_BINBYTES = 0x43 // C

/* protocol 4 */
const OP_SHORT_BINUNICODE = 0x8c
const OP_BINUNICODE8 = 0x8d
const OP_BINBYTES8 = 0x8e
const OP_EMPTY_SET = 0x8f
// 这两个的顺序与直觉相反（EMPTY_SET 之后是 ADDITEMS 才轮到 FROZENSET），
// 是照 `pickletools.dis` 的实际输出定的，不是照名字排的
const OP_ADDITEMS = 0x90
const OP_FROZENSET = 0x91
const OP_NEWOBJ_EX = 0x92
const OP_STACK_GLOBAL = 0x93
const OP_MEMOIZE = 0x94
const OP_FRAME = 0x95

/* protocol 5 */
const OP_BYTEARRAY8 = 0x96
const OP_NEXT_BUFFER = 0x97
const OP_READONLY_BUFFER = 0x98

const MAX_PROTOCOL = 5

/* ── 上限。都是防御，真实数据碰不到 ───────────────────── */

/**
 * 嵌套深度，与 `msgpack.ts` / `php.ts` 同为 256。
 *
 * 这里的解析器**是迭代的栈机，不递归**，所以炸栈的不是它 —— 是拿到产物之后
 * 递归遍历那棵树的人（`render.ts` 自己另有 32 层上限）。上限设在这里是为了
 * 让"树太深"在解析阶段就报出来，而不是变成某个下游的 `RangeError`。
 */
const MAX_DEPTH = 256

/** 节点总数。1M 个节点已经是几十 MB 的内存，而真实序列化值差着几个数量级。 */
const MAX_NODES = 1_000_000

/** `MARK` 的层数。`((((((…` 是一段合法但无限增长的输入。 */
const MAX_MARKS = 100_000

/** protocol 0 的文本参数（数字、模块名）最长多少字节，防止在垃圾输入上扫到末尾。 */
const MAX_LINE = 4096

const UTF8_DECODER = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true })

/**
 * 不是 Python 名字的那些，一律加 `&` 前缀。
 *
 * **Python 的标识符不可能含 `&`**（模块名与类名都是标识符，用 `.` 分隔），
 * 所以这些标记与真实的 `module.Class` 不会撞车。判据与 `php.ts` 的 `REF_MARK`
 * 是同一条。
 *
 *   `&memo:3`   memo 引用，见 `loadGet`
 *   `&ext:5`    `EXT1/2/4` 的扩展注册表编号 —— 名字在 unpickler 那边的注册表里，
 *               字节流里只有一个数字，我们**没有**那张表
 *   `&persid`   持久化 ID，由 unpickler 的 `persistent_load` 解释
 */
const MARK_PREFIX = '&'

/* ── 字节读取 ─────────────────────────────────────────── */

class Reader {
  private readonly src: Uint8Array
  private at = 0

  constructor(src: Uint8Array) {
    this.src = src
  }

  /** 错误信息直接显示给用户，所以要说清"在哪里""期待什么"。 */
  fail(message: string): never {
    throw new Error(`不是 pickle 数据（偏移 ${this.at}）：${message}`)
  }

  get offset(): number {
    return this.at
  }

  get done(): boolean {
    return this.at >= this.src.length
  }

  get remaining(): number {
    return this.src.length - this.at
  }

  byte(): number {
    if (this.at >= this.src.length) this.fail('数据到这里就没了')
    const value = this.src[this.at]
    this.at += 1
    return value
  }

  /**
   * 取 `len` 个原始字节。
   *
   * **先验够不够再切**，这一条是硬要求：`BINBYTES` 的长度字段是 4 字节，
   * 能声明 4 GB。先 `new Uint8Array(len)` 再填，恶意输入当场就是一次 OOM。
   *
   * `slice` 而不是 `subarray`：不能让节点吊着整段输入的引用。
   */
  bytes(len: number): Uint8Array {
    if (len > this.remaining) {
      this.fail(`声明了 ${len} 个字节，但只剩 ${this.remaining} 个`)
    }
    const out = this.src.slice(this.at, this.at + len)
    this.at += len
    return out
  }

  /** 小端无符号整数，1 / 2 / 4 字节。 */
  uint(width: number): number {
    const raw = this.bytes(width)
    let value = 0
    for (let i = width - 1; i >= 0; i -= 1) value = value * 256 + raw[i]
    return value
  }

  /**
   * 小端 8 字节长度。用 `bigint` 读再收成 `number`：直接按 `number` 累乘会在
   * 2^53 以上悄悄丢精度，而这个值紧接着就要当长度用。
   */
  uint64(): number {
    const raw = this.bytes(8)
    let value = 0n
    for (let i = 7; i >= 0; i -= 1) value = (value << 8n) | BigInt(raw[i])
    if (value > BigInt(this.remaining)) {
      this.fail(`声明了 ${value} 个字节，但只剩 ${this.remaining} 个`)
    }
    return Number(value)
  }

  /** 小端 4 字节**有符号**（`BININT`）。 */
  int32(): number {
    const value = this.uint(4)
    return value >= 0x80000000 ? value - 0x100000000 : value
  }

  /** protocol 0 的参数：读到 `\n` 为止，不含 `\n`。 */
  line(what: string): Uint8Array {
    const start = this.at
    while (this.at < this.src.length && this.src[this.at] !== 0x0a) {
      if (this.at - start > MAX_LINE) this.fail(`${what}太长`)
      this.at += 1
    }
    if (this.at >= this.src.length) this.fail(`${what}没有以换行结束`)
    const out = this.src.slice(start, this.at)
    this.at += 1 // 吃掉 \n
    return out
  }

  /** 文本参数当 ASCII 用：数字、`true`/`false` 之类。类名与内容不能走这条。 */
  asciiLine(what: string): string {
    const raw = this.line(what)
    let out = ''
    for (let i = 0; i < raw.length; i += 1) out += String.fromCharCode(raw[i])
    return out
  }
}

/* ── 值的解释 ─────────────────────────────────────────── */

/**
 * `LONG1` / `LONG4` 的内容：**小端补码**的任意精度整数，空字节串是 0。
 *
 * Python 的 int 没有位宽，所以这里也不设上限 —— `bigint` 装得下，
 * 而长度已经被输入长度卡住了。
 */
function decodeLong(raw: Uint8Array): bigint {
  if (raw.length === 0) return 0n
  let value = 0n
  for (let i = raw.length - 1; i >= 0; i -= 1) value = (value << 8n) | BigInt(raw[i])
  // 最高字节的最高位是符号位
  if (raw[raw.length - 1] & 0x80) value -= 1n << BigInt(raw.length * 8)
  return value
}

/**
 * 内容是 UTF-8 就当文本，否则当字节。
 *
 * 判据与 `php.ts` 的 `stringNode` 一致：塞一串 U+FFFD 进去等于篡改数据。
 * pickle 这边多一层理由 —— Python 3 的 `str` 允许含 surrogate（`BINUNICODE`
 * 写出来是 `surrogatepass` 编码的字节，不是合法 UTF-8），那种值退成 `bytes`
 * 是**如实**的：屏幕上会是十六进制，而它确实就是那几个字节。
 */
function textOrBytes(raw: Uint8Array): Node {
  try {
    return { t: 'str', v: UTF8_DECODER.decode(raw) }
  } catch {
    return { t: 'bytes', v: raw }
  }
}

/**
 * `STRING`（protocol 0）的内容：Python `repr` 风格的引号串，含反斜杠转义。
 *
 * 布局出自 `pickletools` 对 `STRING` 的说明 —— 参数必须带引号，内容按
 * `codecs.escape_decode` 解。这里实现的是那套转义的**字节语义**：
 * `\\` `\'` `\"` `\n` `\t` `\r` `\a` `\b` `\f` `\v`、`\xNN`、以及 1–3 位八进制。
 * 不认识的转义**原样保留反斜杠**（Python 也是这么做的，不报错）。
 */
function unescapeString(raw: Uint8Array, reader: Reader): Uint8Array {
  if (raw.length < 2 || raw[0] !== raw[raw.length - 1] || (raw[0] !== 0x27 && raw[0] !== 0x22)) {
    reader.fail('STRING 的参数必须用引号括起来')
  }
  const body = raw.subarray(1, raw.length - 1)
  const out: number[] = []

  for (let i = 0; i < body.length; i += 1) {
    if (body[i] !== 0x5c) {
      out.push(body[i])
      continue
    }
    i += 1
    if (i >= body.length) reader.fail('STRING 的内容以一个落单的反斜杠结束')

    const escape = body[i]
    const simple = SIMPLE_ESCAPES[escape]
    if (simple !== undefined) {
      out.push(simple)
      continue
    }
    if (escape === 0x78) {
      // \xNN —— 恰好两位，位数不够是错的
      const hi = hexValue(body[i + 1])
      const lo = hexValue(body[i + 2])
      if (hi < 0 || lo < 0) reader.fail('`\\x` 后面应该是两位十六进制')
      out.push((hi << 4) | lo)
      i += 2
      continue
    }
    if (escape >= 0x30 && escape <= 0x37) {
      // 八进制，最多三位
      let value = escape - 0x30
      let taken = 1
      while (taken < 3 && body[i + 1] >= 0x30 && body[i + 1] <= 0x37) {
        value = value * 8 + (body[i + 1] - 0x30)
        i += 1
        taken += 1
      }
      out.push(value & 0xff)
      continue
    }
    // 不认识的转义：反斜杠留着，跟 Python 一致
    out.push(0x5c, escape)
  }

  return Uint8Array.from(out)
}

/**
 * 单字符转义。**`\0` 不在这张表里**，它必须走八进制那条路 ——
 * `\012` 是一个字节（0x0a），在这里拦截会把它解成 `\0` 再跟两个字面数字。
 */
const SIMPLE_ESCAPES: Record<number, number> = {
  0x5c: 0x5c, // \\
  0x27: 0x27, // \'
  0x22: 0x22, // \"
  0x6e: 0x0a, // \n
  0x74: 0x09, // \t
  0x72: 0x0d, // \r
  0x61: 0x07, // \a
  0x62: 0x08, // \b
  0x66: 0x0c, // \f
  0x76: 0x0b, // \v
}

function hexValue(byte: number | undefined): number {
  if (byte === undefined) return -1
  if (byte >= 0x30 && byte <= 0x39) return byte - 0x30
  if (byte >= 0x61 && byte <= 0x66) return byte - 0x61 + 10
  if (byte >= 0x41 && byte <= 0x46) return byte - 0x41 + 10
  return -1
}

/**
 * `UNICODE`（protocol 0）的内容：`raw-unicode-escape` 编码。
 *
 * 那个编码的语义是"`\uXXXX` / `\UXXXXXXXX` 按转义解，其余字节按 latin-1"——
 * 注意它**不**处理 `\n` 这类转义（"raw" 就是这个意思），所以不能拿
 * `unescapeString` 凑合。
 */
function decodeRawUnicodeEscape(raw: Uint8Array, reader: Reader): string {
  let out = ''
  for (let i = 0; i < raw.length; i += 1) {
    if (raw[i] !== 0x5c) {
      out += String.fromCharCode(raw[i]) // latin-1
      continue
    }
    const kind = raw[i + 1]
    const width = kind === 0x75 ? 4 : kind === 0x55 ? 8 : 0
    if (width === 0) {
      out += '\\'
      continue
    }
    let code = 0
    for (let d = 0; d < width; d += 1) {
      const digit = hexValue(raw[i + 2 + d])
      if (digit < 0) reader.fail(`\`\\${String.fromCharCode(kind)}\` 后面应该是 ${width} 位十六进制`)
      code = code * 16 + digit
    }
    if (code > 0x10ffff) reader.fail('码位超出 Unicode 范围')
    // 单独的 surrogate 码位是合法的 Python str，String.fromCodePoint 也接受
    out += String.fromCodePoint(code)
    i += 1 + width
  }
  return out
}

/* ── 栈机 ─────────────────────────────────────────────── */

class Machine {
  private readonly reader: Reader
  private readonly stack: Node[] = []
  private readonly marks: number[] = []
  private readonly memo = new Map<number, Node>()
  private budget = MAX_NODES

  constructor(reader: Reader) {
    this.reader = reader
  }

  private fail(message: string): never {
    return this.reader.fail(message)
  }

  /** 每造一个节点扣一次预算。容器不额外计费，它的元素各自计过了。 */
  private alloc<T extends Node>(node: T): T {
    this.budget -= 1
    if (this.budget < 0) this.fail(`节点数超过 ${MAX_NODES}`)
    return node
  }

  private push(node: Node): void {
    this.stack.push(node)
  }

  private pop(): Node {
    const node = this.stack.pop()
    if (node === undefined) this.fail('栈空了，但这个 opcode 还要取一个值')
    if (this.stack.length < (this.marks[this.marks.length - 1] ?? 0)) {
      this.fail('这个 opcode 取值时越过了 MARK')
    }
    return node
  }

  private peek(): Node {
    const node = this.stack[this.stack.length - 1]
    if (node === undefined) this.fail('栈空了，但这个 opcode 还要看一眼栈顶')
    return node
  }

  /** 弹出最近一个 `MARK` 之后的全部值，`MARK` 本身一并消掉。 */
  private popMark(): Node[] {
    const base = this.marks.pop()
    if (base === undefined) this.fail('这里需要一个 MARK，但前面没有')
    return this.stack.splice(base)
  }

  private memoPut(index: number, node: Node): void {
    this.memo.set(index, node)
  }

  /**
   * memo 引用**不展开成共享子树**，记一个标记。
   *
   * 判据与 `php.ts` 处理 `R:` / `r:` 完全一样：展开会让产物不再是一棵树 ——
   * 一个 `a = [大对象]; [a] * 100` 会把展示撑爆，而自引用容器（`l = []`;
   * `l.append(l)`）直接就是个环。
   *
   * 而且标记**信息更多**：屏幕上的 `&memo:3` 说的是"这里和前面是同一个对象"，
   * 展开成副本反而会让人以为是两个恰好相等的值。
   *
   * 取不到就抛错，不给默认值 —— 一个 `undefined` 混进树里比失败难查得多。
   */
  private loadGet(index: number): void {
    if (!this.memo.has(index)) this.fail(`memo 里没有编号 ${index}`)
    this.push(this.alloc({ t: 'call', fn: `${MARK_PREFIX}memo:${index}`, v: null }))
  }

  /**
   * `APPENDS` / `SETITEMS` / `ADDITEMS` 的落点。
   *
   * 目标是普通容器时直接往里塞；是 `REDUCE` 出来的对象（`call`）时塞进它的
   * `data`。后者不是边角情况 —— `collections.OrderedDict` 就是
   * `STACK_GLOBAL → REDUCE → MARK … SETITEMS` 这么写出来的，见 types.ts。
   */
  private emptyContainer(kind: 'arr' | 'map' | 'set'): Node {
    if (kind === 'arr') return { t: 'arr', v: [] }
    if (kind === 'map') return { t: 'map', v: [] }
    return { t: 'seq', cls: 'set', v: [] }
  }

  private containerFor(kind: 'arr' | 'map' | 'set'): Node {
    const target = this.peek()
    if (target.t !== 'call') return target
    // 先存进局部再赋值：`target.data` 是可选属性，赋值之后 TS 的收窄会被
    // 中间那次方法调用作废，直接 `return target.data` 拿到的是 `Node | undefined`
    const container = target.data ?? this.alloc(this.emptyContainer(kind))
    target.data = container
    return container
  }

  /** `push(...items)` 会在 items 上万时把参数栈撑爆，容器操作一律用循环。 */
  private appendInto(items: Node[]): void {
    const target = this.containerFor('arr')
    if (target.t !== 'arr') this.fail(`APPEND 的目标应该是一个 list，实际是 ${target.t}`)
    for (const item of items) target.v.push(item)
  }

  private setItemsInto(items: Node[]): void {
    if (items.length % 2 !== 0) this.fail('SETITEMS 的键与值没有配成对')
    const target = this.containerFor('map')
    if (target.t !== 'map') this.fail(`SETITEM 的目标应该是一个 dict，实际是 ${target.t}`)
    for (let i = 0; i < items.length; i += 2) target.v.push([items[i], items[i + 1]])
  }

  private addItemsInto(items: Node[]): void {
    const target = this.containerFor('set')
    if (target.t !== 'seq') this.fail(`ADDITEMS 的目标应该是一个 set，实际是 ${target.t}`)
    for (const item of items) target.v.push(item)
  }

  /**
   * `BUILD`：把 state 补到栈顶对象上。
   *
   * 栈顶必须是 `call` —— `BUILD` 的语义是 `inst.__setstate__(state)`，退化路径
   * 是 `inst.__dict__.update(state)`，两者都要求 `inst` 是个实例。栈顶是
   * `EMPTY_DICT` 那样的东西时**抛错**：那种流 Python 自己 unpickle 也会失败
   * （`dict` 既没有 `__setstate__` 也没有 `__dict__`），拒绝是忠实而不是偷懒。
   */
  private loadBuild(): void {
    const state = this.pop()
    const target = this.peek()
    if (target.t !== 'call') {
      this.fail(`BUILD 的目标应该是一个对象，实际是 ${target.t}`)
    }
    if (target.state !== undefined) this.fail('同一个对象上有两次 BUILD')
    target.state = state
  }

  /** `GLOBAL` / `STACK_GLOBAL` 解出来的名字，**不解析、不加载**。 */
  private globalName(moduleName: string, attrName: string): Node {
    return this.alloc({ t: 'call', fn: `${moduleName}.${attrName}`, v: null })
  }

  /** 把一个引用变成一次调用。`v: null → Node[]` 正是"没被调用 → 被调用了"。 */
  private callWith(callee: Node, args: Node[]): Node {
    if (callee.t !== 'call' || callee.v !== null) {
      this.fail(`这里应该是一个可调用对象，实际是 ${callee.t}`)
    }
    return this.alloc({ t: 'call', fn: callee.fn, v: args })
  }

  private textFromStack(node: Node, what: string): string {
    if (node.t !== 'str') this.fail(`${what}应该是字符串，实际是 ${node.t}`)
    return node.v
  }

  run(): Node {
    for (;;) {
      const op = this.reader.byte()
      if (op === OP_STOP) break
      this.step(op)
    }

    // 消费完整性，两条一起断言，见文件头。顺序要紧：先说栈的事，
    // 因为"栈上不止一个值"往往是解析走偏的**因**，尾部有剩是它的果
    if (this.stack.length !== 1) {
      this.fail(`STOP 时栈上有 ${this.stack.length} 个值，应该恰好一个`)
    }
    if (!this.reader.done) {
      this.fail(`STOP 之后还剩 ${this.reader.remaining} 个字节没有解释`)
    }
    return this.stack[0]
  }

  private step(op: number): void {
    switch (op) {
      /* ── 协议与分帧 ── */
      case OP_PROTO: {
        const version = this.reader.byte()
        if (version > MAX_PROTOCOL) this.fail(`protocol ${version} 还不认识`)
        return
      }
      case OP_FRAME: {
        // 只校验它不越过输入末尾（uint64 自己会验），不强求 opcode 恰好填满这一帧：
        // 跨帧读只是"写得不标准"，不构成安全问题，而严格对齐要在每次读取时都判一道
        this.reader.uint64()
        return
      }

      /* ── 常量 ── */
      case OP_NONE:
        return this.push(this.alloc({ t: 'null' }))
      case OP_NEWTRUE:
        return this.push(this.alloc({ t: 'bool', v: true }))
      case OP_NEWFALSE:
        return this.push(this.alloc({ t: 'bool', v: false }))

      /* ── 整数 ── */
      case OP_INT: {
        // `I01\n` / `I00\n` 是 protocol 0 的 True / False —— 它们**不是**整数 1 和 0，
        // 解成 int 会把类型悄悄改掉
        const text = this.reader.asciiLine('INT 的参数')
        if (text === '01') return this.push(this.alloc({ t: 'bool', v: true }))
        if (text === '00') return this.push(this.alloc({ t: 'bool', v: false }))
        return this.push(this.alloc({ t: 'int', v: this.intFromText(text, 'INT') }))
      }
      case OP_LONG: {
        // 尾部的 `L` 是 Python 2 的 long 后缀，可有可无
        const text = this.reader.asciiLine('LONG 的参数')
        const body = text.endsWith('L') ? text.slice(0, -1) : text
        return this.push(this.alloc({ t: 'int', v: this.intFromText(body, 'LONG') }))
      }
      case OP_BININT:
        return this.push(this.alloc({ t: 'int', v: BigInt(this.reader.int32()) }))
      case OP_BININT1:
        return this.push(this.alloc({ t: 'int', v: BigInt(this.reader.uint(1)) }))
      case OP_BININT2:
        return this.push(this.alloc({ t: 'int', v: BigInt(this.reader.uint(2)) }))
      case OP_LONG1:
        return this.push(this.alloc({ t: 'int', v: decodeLong(this.reader.bytes(this.reader.uint(1))) }))
      case OP_LONG4:
        return this.push(this.alloc({ t: 'int', v: decodeLong(this.reader.bytes(this.reader.uint(4))) }))

      /* ── 浮点 ── */
      case OP_FLOAT: {
        // 不填 `literal`：那个字段是给往返自检用的（同一个 double 有多种写法），
        // 而这里根本没有 encode 那一侧。见 types.ts
        const text = this.reader.asciiLine('FLOAT 的参数')
        return this.push(this.alloc({ t: 'float', v: this.floatFromText(text), bits: 64 }))
      }
      case OP_BINFLOAT: {
        const raw = this.reader.bytes(8)
        // pickle 的 BINFLOAT 是**大端**，跟其余定宽字段的小端相反
        const view = new DataView(raw.buffer, raw.byteOffset, 8)
        return this.push(this.alloc({ t: 'float', v: view.getFloat64(0, false), bits: 64 }))
      }

      /* ── 字符串与字节 ── */
      case OP_STRING: {
        const raw = unescapeString(this.reader.line('STRING 的参数'), this.reader)
        return this.push(this.alloc(textOrBytes(raw)))
      }
      case OP_BINSTRING:
        return this.push(this.alloc(textOrBytes(this.reader.bytes(this.reader.uint(4)))))
      case OP_SHORT_BINSTRING:
        return this.push(this.alloc(textOrBytes(this.reader.bytes(this.reader.uint(1)))))
      case OP_UNICODE: {
        const text = decodeRawUnicodeEscape(this.reader.line('UNICODE 的参数'), this.reader)
        return this.push(this.alloc({ t: 'str', v: text }))
      }
      case OP_BINUNICODE:
        return this.push(this.alloc(textOrBytes(this.reader.bytes(this.reader.uint(4)))))
      case OP_SHORT_BINUNICODE:
        return this.push(this.alloc(textOrBytes(this.reader.bytes(this.reader.uint(1)))))
      case OP_BINUNICODE8:
        return this.push(this.alloc(textOrBytes(this.reader.bytes(this.reader.uint64()))))
      case OP_BINBYTES:
        return this.push(this.alloc({ t: 'bytes', v: this.reader.bytes(this.reader.uint(4)) }))
      case OP_SHORT_BINBYTES:
        return this.push(this.alloc({ t: 'bytes', v: this.reader.bytes(this.reader.uint(1)) }))
      case OP_BINBYTES8:
        return this.push(this.alloc({ t: 'bytes', v: this.reader.bytes(this.reader.uint64()) }))
      case OP_BYTEARRAY8:
        // bytearray 与 bytes 在 Python 里是两个类型，但都只是一段字节；
        // 把可变性标出来没有意义，屏幕上都是十六进制
        return this.push(this.alloc({ t: 'bytes', v: this.reader.bytes(this.reader.uint64()) }))

      /* ── 容器 ── */
      case OP_EMPTY_LIST:
        return this.push(this.alloc({ t: 'arr', v: [] }))
      case OP_EMPTY_DICT:
        return this.push(this.alloc({ t: 'map', v: [] }))
      case OP_EMPTY_SET:
        return this.push(this.alloc({ t: 'seq', cls: 'set', v: [] }))
      case OP_EMPTY_TUPLE:
        return this.push(this.alloc({ t: 'seq', cls: 'tuple', v: [] }))
      case OP_TUPLE1:
        return this.pushTuple(1)
      case OP_TUPLE2:
        return this.pushTuple(2)
      case OP_TUPLE3:
        return this.pushTuple(3)
      case OP_TUPLE:
        return this.push(this.alloc({ t: 'seq', cls: 'tuple', v: this.popMark() }))
      case OP_LIST:
        return this.push(this.alloc({ t: 'arr', v: this.popMark() }))
      case OP_FROZENSET:
        return this.push(this.alloc({ t: 'seq', cls: 'frozenset', v: this.popMark() }))
      case OP_DICT: {
        const items = this.popMark()
        if (items.length % 2 !== 0) this.fail('DICT 的键与值没有配成对')
        const pairs: Array<[Node, Node]> = []
        for (let i = 0; i < items.length; i += 2) pairs.push([items[i], items[i + 1]])
        return this.push(this.alloc({ t: 'map', v: pairs }))
      }

      /* ── 往容器里塞 ── */
      case OP_APPEND:
        return this.appendInto([this.pop()])
      case OP_APPENDS:
        return this.appendInto(this.popMark())
      case OP_SETITEM: {
        const value = this.pop()
        const key = this.pop()
        return this.setItemsInto([key, value])
      }
      case OP_SETITEMS:
        return this.setItemsInto(this.popMark())
      case OP_ADDITEMS:
        return this.addItemsInto(this.popMark())

      /* ── memo ── */
      case OP_PUT:
        return this.memoPut(this.parseIndex(this.reader.asciiLine('PUT 的参数')), this.peek())
      case OP_BINPUT:
        return this.memoPut(this.reader.uint(1), this.peek())
      case OP_LONG_BINPUT:
        return this.memoPut(this.reader.uint(4), this.peek())
      case OP_MEMOIZE:
        // 编号是**当前 memo 的条数**，不是流里的某个数字
        return this.memoPut(this.memo.size, this.peek())
      case OP_GET:
        return this.loadGet(this.parseIndex(this.reader.asciiLine('GET 的参数')))
      case OP_BINGET:
        return this.loadGet(this.reader.uint(1))
      case OP_LONG_BINGET:
        return this.loadGet(this.reader.uint(4))

      /* ── 栈操作 ── */
      case OP_MARK: {
        if (this.marks.length >= MAX_MARKS) this.fail(`MARK 超过 ${MAX_MARKS} 层`)
        this.marks.push(this.stack.length)
        return
      }
      case OP_POP: {
        this.pop()
        return
      }
      case OP_POP_MARK: {
        this.popMark()
        return
      }
      case OP_DUP: {
        // 压入**同一个引用**，那正是 Python 的语义（DUP 之后 APPEND，两处一起变）。
        // pickle 自己的 Pickler 从不生成这个 opcode，所以只有手写的流才会到这里；
        // 由此产生的共享（乃至环）靠 `assertDepth` 的 visited 与 render.ts 的
        // 深度上限兜住
        return this.push(this.peek())
      }

      /* ── 对象构造。这几个就是"反序列化即执行"的那几个 ── */
      case OP_GLOBAL: {
        const moduleName = this.reader.asciiLine('GLOBAL 的模块名')
        const attrName = this.reader.asciiLine('GLOBAL 的属性名')
        return this.push(this.globalName(moduleName, attrName))
      }
      case OP_STACK_GLOBAL: {
        const attrName = this.textFromStack(this.pop(), 'STACK_GLOBAL 的属性名')
        const moduleName = this.textFromStack(this.pop(), 'STACK_GLOBAL 的模块名')
        return this.push(this.globalName(moduleName, attrName))
      }
      case OP_REDUCE: {
        const args = this.pop()
        const callee = this.pop()
        if (args.t !== 'seq' || args.cls !== 'tuple') {
          this.fail(`REDUCE 的参数应该是一个 tuple，实际是 ${args.t}`)
        }
        return this.push(this.callWith(callee, args.v))
      }
      case OP_NEWOBJ: {
        const args = this.pop()
        const cls = this.pop()
        if (args.t !== 'seq' || args.cls !== 'tuple') {
          this.fail(`NEWOBJ 的参数应该是一个 tuple，实际是 ${args.t}`)
        }
        return this.push(this.callWith(cls, args.v))
      }
      case OP_NEWOBJ_EX: {
        const kwargs = this.pop()
        const args = this.pop()
        const cls = this.pop()
        if (args.t !== 'seq' || args.cls !== 'tuple') {
          this.fail(`NEWOBJ_EX 的参数应该是一个 tuple，实际是 ${args.t}`)
        }
        // 关键字参数摆在位置参数后面，用一个 map 节点兜着 —— 丢掉它等于
        // 隐去了一部分"加载时会拿什么去调用"
        return this.push(this.callWith(cls, [...args.v, kwargs]))
      }
      case OP_INST: {
        const moduleName = this.reader.asciiLine('INST 的模块名')
        const attrName = this.reader.asciiLine('INST 的属性名')
        const args = this.popMark()
        return this.push(this.alloc({ t: 'call', fn: `${moduleName}.${attrName}`, v: args }))
      }
      case OP_OBJ: {
        // MARK 之后第一个是类，其余是参数
        const items = this.popMark()
        const callee = items.shift()
        if (callee === undefined) this.fail('OBJ 后面没有类')
        return this.push(this.callWith(callee, items))
      }
      case OP_BUILD:
        return this.loadBuild()

      /* ── 引用类：名字不在字节流里 ── */
      case OP_PERSID: {
        const id = this.reader.asciiLine('PERSID 的参数')
        return this.push(
          this.alloc({
            t: 'call',
            fn: `${MARK_PREFIX}persid`,
            v: [this.alloc({ t: 'str', v: id })],
          }),
        )
      }
      case OP_BINPERSID:
        return this.push(this.alloc({ t: 'call', fn: `${MARK_PREFIX}persid`, v: [this.pop()] }))
      case OP_EXT1:
        return this.pushExt(this.reader.uint(1))
      case OP_EXT2:
        return this.pushExt(this.reader.uint(2))
      case OP_EXT4:
        return this.pushExt(this.reader.uint(4))

      /* ── protocol 5 的带外缓冲：数据根本不在这段字节里 ── */
      case OP_NEXT_BUFFER:
      case OP_READONLY_BUFFER:
        this.fail(
          'protocol 5 的带外缓冲（out-of-band buffer）把数据放在字节流之外，' +
            '需要额外的传输通道才能读 —— Redis 里存不下这种值',
        )
    }

    this.fail(`没有 0x${op.toString(16).padStart(2, '0')} 这个 opcode`)
  }

  private pushTuple(count: number): void {
    const items: Node[] = []
    for (let i = 0; i < count; i += 1) items.unshift(this.pop())
    this.push(this.alloc({ t: 'seq', cls: 'tuple', v: items }))
  }

  private pushExt(code: number): void {
    // 名字在 unpickler 那边的 copyreg 扩展注册表里，字节流里只有一个编号。
    // 编不出名字就别编，把编号如实摆出来
    this.push(this.alloc({ t: 'call', fn: `${MARK_PREFIX}ext:${code}`, v: null }))
  }

  private intFromText(text: string, what: string): bigint {
    if (!INT_TEXT.test(text)) this.fail(`\`${text}\` 不是 ${what} 该有的整数`)
    return BigInt(text)
  }

  /**
   * `repr(float)` 写出来的形态。
   *
   * 三个非有限值**必须特判**：Python 写的是 `inf` / `-inf` / `nan`，
   * 而 JS 的 `Number('inf')` 是 `NaN` —— 照着 `Number()` 走会把正无穷悄悄
   * 变成 NaN，那是一次静默的值改写。
   */
  private floatFromText(text: string): number {
    if (text === 'inf') return Number.POSITIVE_INFINITY
    if (text === '-inf') return Number.NEGATIVE_INFINITY
    if (text === 'nan') return Number.NaN
    if (!FLOAT_TEXT.test(text)) this.fail(`\`${text}\` 不是一个浮点字面量`)
    return Number(text)
  }

  private parseIndex(text: string): number {
    if (!INDEX_TEXT.test(text)) this.fail(`\`${text}\` 不是一个 memo 编号`)
    return Number(text)
  }
}

/** 只认十进制（可带符号）。Python 那边 `int(x, 0)` 还认 `0x`，但真实数据里没有。 */
const INT_TEXT = /^[+-]?\d+$/
const INDEX_TEXT = /^\d+$/
/** 有限值的写法。`inf` / `-inf` / `nan` 在 `floatFromText` 里单独认。 */
const FLOAT_TEXT = /^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/

/**
 * 嵌套深度检查。
 *
 * **自己是迭代的**：一个用来防止别人炸栈的函数，不能先把自己栈溢出了。
 *
 * `visited` 不是为了检测环，是为了不在共享子树上重复遍历（`DUP` 会造出共享，
 * 见那个 case）。真有环时它同时兜住了死循环 —— 环上的节点第二次遇到就跳过，
 * 而那种流本来也只能是手写出来的。
 */
function assertDepth(root: Node): void {
  const visited = new Set<Node>()
  const pending: Array<[Node, number]> = [[root, 0]]

  while (pending.length > 0) {
    const [node, depth] = pending.pop() as [Node, number]
    if (depth > MAX_DEPTH) throw new Error(`不是 pickle 数据：嵌套超过 ${MAX_DEPTH} 层`)
    if (visited.has(node)) continue
    visited.add(node)

    switch (node.t) {
      case 'arr':
      case 'seq':
        for (const child of node.v) pending.push([child, depth + 1])
        break
      case 'map':
      case 'obj':
        for (const [key, value] of node.v) {
          pending.push([key, depth + 1], [value, depth + 1])
        }
        break
      case 'call':
        if (node.v) for (const child of node.v) pending.push([child, depth + 1])
        if (node.state) pending.push([node.state, depth + 1])
        if (node.data) pending.push([node.data, depth + 1])
        break
      case 'field':
        pending.push([node.v, depth + 1])
        break
      default:
        break
    }
  }
}

function decode(bytes: Uint8Array): Node {
  const root = new Machine(new Reader(bytes)).run()
  assertDepth(root)
  return root
}

/**
 * 自动识别的额外前置约束：**必须是 protocol 2+ 的 `PROTO` 前缀**。
 *
 * "能解开"这一条对 pickle 远远不够 —— `N.` 两个字节就是一段合法 pickle
 * （NONE + STOP），任何以合法 opcode 开头、恰好以 `.` 结尾的短值都会被认领。
 * protocol 0 / 1 没有 `PROTO` opcode，前缀就是任意 opcode 字节，放宽这一条
 * 等于重蹈 msgpack 那条覆辙。
 *
 * **这只挡自动识别，不挡手点** —— protocol 0 / 1 的数据照样解得开。
 * 两个维度独立，见 doc/frontend/codecs.md「识别的三个维度」。
 */
function detectPrefix(bytes: Uint8Array): boolean {
  return bytes.length >= 2 && bytes[0] === OP_PROTO && bytes[1] >= 2 && bytes[1] <= MAX_PROTOCOL
}

/**
 * 注册进 `READONLY_CODECS.pickle` 的那一项。
 *
 * **只有 `decode`** —— 这是 `ReadonlyCodec` 与 `RendererCodec` 的全部区别，
 * 也是 doc/backend/codec.md「Readonly 安全边界」的落点。谁想给它补一个 `encode`，先读那一节：
 * 拦着的不是"我们还没写"，是"写回等于制造一份会被别人加载并执行的载荷"。
 */
export const pickleCodec: ReadonlyCodec = { decode, detectPrefix }
