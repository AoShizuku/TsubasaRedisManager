import type { RendererCodec } from './index'
import type { Node } from './types'

/**
 * PHP `serialize()` 的解析与重编码。
 *
 * ## 为什么自写（doc/frontend/codec-formats.md「PHP serialize」）
 *
 * JS 生态的 unserialize 类库普遍搞错**字节语义**：`s:5:"héllo"` 里的 5 是字节数，
 * 而库把输入先 `TextDecoder` 成 JS string 再 parse，长度就成了 UTF-16 code unit 数，
 * 非 ASCII 立刻错位。我们的输入本来就是 `Uint8Array`，所以这里**全程按字节走**：
 * 只有 `s:` 的内容在确认是合法 UTF-8 时才解成 JS string，否则原样进 `bytes` 节点
 * —— PHP 的 string 是字节串，不保证 UTF-8，塞一串 U+FFFD 进去等于篡改数据。
 *
 * ## 格式出处
 *
 * 独立编写，未参考也未移植任何现成实现（CLAUDE.md 的来源约束）。对照的是格式
 * 规范与 PHP 官方实现的**可观察行为**，不是它们的代码：
 *
 *   - PHP 手册 `serialize()` / `unserialize()` 及 "PHP Serialization Format"：
 *     `N;` `b:` `i:` `d:` `s:` `a:` `O:` `r:` `R:`，`Serializable` 的 `C:`，
 *     PHP 8.1 起 enum 的 `E:`
 *   - PHP 官方实现 `ext/standard/var_unserializer.re` 的 token 文法（读的一侧）：
 *     `iv = [+-]? [0-9]+`、`uiv = [+]? [0-9]+`、`d:` 还接受 `NAN` / `INF` / `-INF`，
 *     `S:` 的内容带 `\xx` 十六进制转义、且长度是**解码后**的字节数，
 *     数组的 key 只接受 int 与 string
 *   - PHP 官方实现 `ext/standard/var.c` 的 `php_var_serialize_intern`（写的一侧）：
 *     它只写 `s:` 从不写 `S:`；`C:` 的内容整段交给类自己写
 *   - float 的写法出自 `main/snprintf.c` 的 `php_gcvt`，见 `phpDoubleText()`
 *
 * ## 严格是刚性要求
 *
 * 解完必须消耗掉全部输入，有剩余就抛错。`index.ts` 的 `detectCodec` 拿
 * "decode 抛不抛错"当自动识别的判据（php 是唯一进候选集的序列化格式），
 * 宽松的解析器会把 `a:1:{i:0;i:1;}` 后面跟着的任意垃圾也判成成功，自动识别就废了。
 * 同理 **decode 抛错 = 不是这个格式**，不要返回"部分解出来"的结构。
 */

/* ── 字节常量。全程按字节比对，不要在中途拼字符串 ───────── */

const CH_QUOTE = 0x22
const CH_PLUS = 0x2b
const CH_MINUS = 0x2d
const CH_ZERO = 0x30
const CH_ONE = 0x31
const CH_NINE = 0x39
const CH_COLON = 0x3a
const CH_SEMICOLON = 0x3b
const CH_BACKSLASH = 0x5c
const CH_LBRACE = 0x7b
const CH_RBRACE = 0x7d

const CH_UPPER_C = 0x43
const CH_UPPER_E = 0x45
const CH_UPPER_N = 0x4e
const CH_UPPER_O = 0x4f
const CH_UPPER_R = 0x52
const CH_UPPER_S = 0x53
const CH_LOWER_A = 0x61
const CH_LOWER_B = 0x62
const CH_LOWER_D = 0x64
const CH_LOWER_I = 0x69
const CH_LOWER_R = 0x72
const CH_LOWER_S = 0x73

/** PHP 的 int 是平台 int；serialize 出来的数据只可能来自 64 位或 32 位平台。 */
const INT64_MIN = -9223372036854775808n
const INT64_MAX = 9223372036854775807n

/**
 * 嵌套上限，纯防御：`a:1:{i:0;a:1:{i:0;…` 这样的输入能让递归下降炸栈，
 * 而炸栈抛的是 `RangeError`，混在"解不开"里看不出到底出了什么事。
 * 真实数据不会有几百层，`render.ts` 展示时也只展开 32 层。
 */
const MAX_DEPTH = 256

/** `d:` 的字面量最长也就二十来字节。设个上限，免得在垃圾输入上一路扫到末尾。 */
const MAX_DOUBLE_TEXT = 64

/** `iv` / `nv` / `nvexp` 三条文法合起来。`Number()` 自己还认 `0x10` 与 `Infinity`，所以要先卡。 */
const DOUBLE_TEXT = /^[+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?$/

/**
 * `ignoreBOM: true` 不是可选项：默认的 `TextDecoder` 会**吃掉**开头的 U+FEFF，
 * 于是 `s:3:"\xEF\xBB\xBF";` 解出空串、重编成 `s:0:"";`，字节悄悄少了三个。
 * `fatal: true` 同理 —— 不合法的 UTF-8 要的结论是"这不是文本"，不是一串 U+FFFD。
 */
const UTF8_DECODER = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true })
const UTF8_ENCODER = new TextEncoder()

/**
 * `Node` 里没有"引用""自定义序列化"这两种变体，而 `types.ts` 是规格不该改形状，
 * 所以它们借 `obj` 的类名位置来表达。这样安全：**PHP 的类名不可能含 `:` 或 `&`**
 * （标识符是 `[a-zA-Z_\x80-\xff][a-zA-Z0-9_\x80-\xff]*`，命名空间用 `\` 分隔），
 * 所以下面三种标记与真实类名不会撞车。
 *
 *   `Suit::Hearts`   ← `E:11:"Suit:Hearts";`，正好是 PHP 源码里写这个 case 的样子
 *   `C:Foo`          ← `C:3:"Foo":n:{…}`，属性只有一个 `__serialized`
 *   `&R:3` / `&r:3`  ← `R:3;` / `r:3;`
 *
 * 后两种 `encode` 会**主动抛错**，理由见 `writeObject()`。
 */
const ENUM_MARK = '::'
const CUSTOM_MARK = 'C:'
const REF_MARK = '&'

/** `C:` 那一格原始字节挂在这个属性名下。它不是 PHP 的东西，是我们摆放位置的标签。 */
const CUSTOM_KEY = '__serialized'

/* ── 解析 ─────────────────────────────────────────────── */

function isDigit(byte: number): boolean {
  return byte >= CH_ZERO && byte <= CH_NINE
}

/** token / 数字 / 转义位都是 ASCII，走这条；类名与字符串内容是任意字节，不能走。 */
function asciiOf(bytes: Uint8Array): string {
  let out = ''
  for (let i = 0; i < bytes.length; i += 1) out += String.fromCharCode(bytes[i])
  return out
}

class Reader {
  private readonly src: Uint8Array
  private at = 0

  constructor(src: Uint8Array) {
    this.src = src
  }

  /** 错误信息会直接显示给用户，所以要说清"在哪里""期待什么"。 */
  fail(message: string): never {
    throw new Error(`不是 PHP serialize 数据（偏移 ${this.at}）：${message}`)
  }

  private peek(): number {
    if (this.at >= this.src.length) this.fail('数据到这里就没了')
    return this.src[this.at]
  }

  private take(): number {
    const byte = this.peek()
    this.at += 1
    return byte
  }

  private expect(byte: number, what: string): void {
    if (this.peek() !== byte) this.fail(`这里应该是 ${what}`)
    this.at += 1
  }

  /** 解完必须一个字节都不剩，见文件头"严格是刚性要求"。 */
  expectEnd(): void {
    if (this.at !== this.src.length) {
      this.fail(`末尾还剩 ${this.src.length - this.at} 个字节没有解释`)
    }
  }

  /** `uiv`：可以带一个 `+`（PHP 的文法就是这么写的），不能带 `-`。 */
  private takeLength(): number {
    if (this.peek() === CH_PLUS) this.at += 1
    const start = this.at
    let value = 0
    while (this.at < this.src.length && isDigit(this.src[this.at])) {
      value = value * 10 + (this.src[this.at] - CH_ZERO)
      // 长度不可能超过整段输入。这一卡兼作溢出保护：不然 `s:99999999999999999999:`
      // 会把 value 撑成 Infinity，后面的边界检查就全成了摆设
      if (value > this.src.length) this.fail('长度比整段数据还长')
      this.at += 1
    }
    if (this.at === start) this.fail('这里应该是一个长度')
    return value
  }

  /** `iv`：`[+-]? [0-9]+`。 */
  private takeInt(): bigint {
    const lead = this.peek()
    const negative = lead === CH_MINUS
    if (lead === CH_PLUS || lead === CH_MINUS) this.at += 1

    const start = this.at
    while (this.at < this.src.length && isDigit(this.src[this.at])) this.at += 1
    if (this.at === start) this.fail('这里应该是一个整数')

    const digits = BigInt(asciiOf(this.src.subarray(start, this.at)))
    const value = negative ? -digits : digits
    // PHP 的 serialize 不可能写出超出 int64 的 i:，所以超了就是"不是这个格式"。
    // 卡住它让自动识别更可靠，代价只是拒绝一份 PHP 自己也读不回去的数据
    if (value < INT64_MIN || value > INT64_MAX) this.fail('整数超出 int64')
    return value
  }

  /** 取 `len` 个原始字节。`slice` 而不是 `subarray`：不能让节点吊着整段输入的引用。 */
  private takeBytes(len: number): Uint8Array {
    if (this.at + len > this.src.length) this.fail(`还差 ${this.at + len - this.src.length} 个字节`)
    const out = this.src.slice(this.at, this.at + len)
    this.at += len
    return out
  }

  /** 类名 / enum 名要当 JS string 用（`Node.obj.cls` 是 string），不合法就别猜。 */
  private takeText(raw: Uint8Array, what: string): string {
    try {
      return UTF8_DECODER.decode(raw)
    } catch {
      this.fail(`${what}不是合法 UTF-8`)
    }
  }

  private takeClassName(): string {
    const len = this.takeLength()
    this.expect(CH_COLON, '`:`')
    this.expect(CH_QUOTE, '`"`')
    const raw = this.takeBytes(len)
    this.expect(CH_QUOTE, '`"`')
    return this.takeText(raw, '类名')
  }

  private takeHexDigit(): number {
    const byte = this.take()
    if (byte >= 0x30 && byte <= 0x39) return byte - 0x30
    if (byte >= 0x61 && byte <= 0x66) return byte - 0x61 + 10
    if (byte >= 0x41 && byte <= 0x46) return byte - 0x41 + 10
    this.at -= 1
    this.fail('`\\` 后面应该是两位十六进制')
  }

  /**
   * `S:` 的内容：`\` + 两位十六进制表示一个字节，其余字节原样。
   * **`len` 数的是解码后的字节数**，所以这里按输出计数循环，不能按输入切片。
   */
  private takeEscaped(len: number): Uint8Array {
    const out = new Uint8Array(len)
    for (let i = 0; i < len; i += 1) {
      const byte = this.take()
      out[i] = byte === CH_BACKSLASH ? (this.takeHexDigit() << 4) | this.takeHexDigit() : byte
    }
    return out
  }

  private readDouble(): Node {
    const start = this.at
    while (this.at < this.src.length && this.src[this.at] !== CH_SEMICOLON) {
      if (this.at - start > MAX_DOUBLE_TEXT) this.fail('浮点字面量太长')
      this.at += 1
    }
    const text = asciiOf(this.src.subarray(start, this.at))
    this.expect(CH_SEMICOLON, '`;`')

    // 这三个是 PHP 单独写死的字面量，不走数字文法（注意没有 `+INF`）
    // `literal` 一律带上：同一个 double 有多种合法写法，重建只能顾一头，见 types.ts
    if (text === 'NAN') return { t: 'float', v: Number.NaN, bits: 64, literal: text }
    if (text === 'INF') {
      return { t: 'float', v: Number.POSITIVE_INFINITY, bits: 64, literal: text }
    }
    if (text === '-INF') {
      return { t: 'float', v: Number.NEGATIVE_INFINITY, bits: 64, literal: text }
    }
    if (!DOUBLE_TEXT.test(text)) this.fail(`\`${text}\` 不是一个浮点字面量`)
    // PHP 的 float 就是 C 的 double，没有 32 位的那一档
    return { t: 'float', v: Number(text), bits: 64, literal: text }
  }

  /** `s:` 与 `S:` 的共同尾巴：内容确认是 UTF-8 才当文本，否则是字节。 */
  private stringNode(raw: Uint8Array): Node {
    this.expect(CH_QUOTE, '`"`')
    this.expect(CH_SEMICOLON, '`;`')
    try {
      return { t: 'str', v: UTF8_DECODER.decode(raw) }
    } catch {
      return { t: 'bytes', v: raw }
    }
  }

  /**
   * PHP 数组的 key 只能是 int 或 string，`unserialize` 自己也拒绝别的。
   * 这里跟着拒绝，一是忠实，二是让自动识别少一条误判路径。
   *
   * 注意**不模仿** PHP 把 `"5"` 这种数字字符串 key 归一成 int 的行为：
   * 那是 PHP 数组的语义，不是序列化格式的语义，归一了就再也编不回原字节。
   */
  private readKey(depth: number): Node {
    const tag = this.peek()
    if (tag !== CH_LOWER_I && tag !== CH_LOWER_S && tag !== CH_UPPER_S) {
      this.fail('数组的 key 只能是整数或字符串')
    }
    return this.readValue(depth)
  }

  private readPairs(count: number, depth: number): Array<[Node, Node]> {
    const pairs: Array<[Node, Node]> = []
    for (let i = 0; i < count; i += 1) {
      const key = this.readKey(depth + 1)
      pairs.push([key, this.readValue(depth + 1)])
    }
    this.expect(CH_RBRACE, '`}`')
    return pairs
  }

  readValue(depth: number): Node {
    if (depth > MAX_DEPTH) this.fail(`嵌套超过 ${MAX_DEPTH} 层`)
    const tag = this.take()

    switch (tag) {
      case CH_UPPER_N: {
        this.expect(CH_SEMICOLON, '`;`')
        return { t: 'null' }
      }

      case CH_LOWER_B: {
        this.expect(CH_COLON, '`:`')
        const digit = this.take()
        if (digit !== CH_ZERO && digit !== CH_ONE) {
          this.at -= 1
          this.fail('布尔只能是 0 或 1')
        }
        this.expect(CH_SEMICOLON, '`;`')
        return { t: 'bool', v: digit === CH_ONE }
      }

      case CH_LOWER_I: {
        this.expect(CH_COLON, '`:`')
        const value = this.takeInt()
        this.expect(CH_SEMICOLON, '`;`')
        return { t: 'int', v: value }
      }

      case CH_LOWER_D: {
        this.expect(CH_COLON, '`:`')
        return this.readDouble()
      }

      case CH_LOWER_S: {
        this.expect(CH_COLON, '`:`')
        const len = this.takeLength() // ← **字节数**，自写这个解析器的全部理由
        this.expect(CH_COLON, '`:`')
        this.expect(CH_QUOTE, '`"`')
        return this.stringNode(this.takeBytes(len))
      }

      case CH_UPPER_S: {
        this.expect(CH_COLON, '`:`')
        const len = this.takeLength()
        this.expect(CH_COLON, '`:`')
        this.expect(CH_QUOTE, '`"`')
        return this.stringNode(this.takeEscaped(len))
      }

      case CH_LOWER_A: {
        this.expect(CH_COLON, '`:`')
        const count = this.takeLength()
        this.expect(CH_COLON, '`:`')
        this.expect(CH_LBRACE, '`{`')
        // PHP 数组的 key 是 int 与 string 混合的**有序**表 —— 所以是 map 不是 arr，
        // 也不能塞进 Record（同时会丢掉类型和顺序），见 types.ts
        return { t: 'map', v: this.readPairs(count, depth) }
      }

      case CH_UPPER_O: {
        this.expect(CH_COLON, '`:`')
        const cls = this.takeClassName()
        this.expect(CH_COLON, '`:`')
        const count = this.takeLength()
        this.expect(CH_COLON, '`:`')
        this.expect(CH_LBRACE, '`{`')
        // 只解出类名与属性，**不实例化任何东西**。PHP 的危险在 PHP 运行时的
        // __wakeup / __destruct，格式本身是纯数据且自描述，见
        // doc/backend/codec.md「Readonly 安全边界」末段
        return { t: 'obj', cls, v: this.readPairs(count, depth) }
      }

      case CH_UPPER_E: {
        this.expect(CH_COLON, '`:`')
        const len = this.takeLength()
        this.expect(CH_COLON, '`:`')
        this.expect(CH_QUOTE, '`"`')
        const raw = this.takeBytes(len)
        this.expect(CH_QUOTE, '`"`')
        this.expect(CH_SEMICOLON, '`;`')

        const payload = this.takeText(raw, 'enum 名')
        // PHP 找的是**最后**一个冒号（类名与 case 名都不可能含冒号）
        const split = payload.lastIndexOf(':')
        if (split < 0) this.fail('enum 名里没有冒号')
        return {
          t: 'obj',
          cls: `${payload.slice(0, split)}${ENUM_MARK}${payload.slice(split + 1)}`,
          v: [],
        }
      }

      case CH_UPPER_C: {
        this.expect(CH_COLON, '`:`')
        const cls = this.takeClassName()
        this.expect(CH_COLON, '`:`')
        const len = this.takeLength()
        this.expect(CH_COLON, '`:`')
        this.expect(CH_LBRACE, '`{`')
        const raw = this.takeBytes(len)
        this.expect(CH_RBRACE, '`}`')
        // 内容是类自己的 Serializable::serialize() 写出来的原始字节，格式不自描述
        // —— 只能当 bytes 摆着。类名要留住，用户得知道这坨字节是谁写的
        return {
          t: 'obj',
          cls: `${CUSTOM_MARK}${cls}`,
          v: [[{ t: 'str', v: CUSTOM_KEY }, { t: 'bytes', v: raw }]],
        }
      }

      case CH_UPPER_R:
      case CH_LOWER_R: {
        this.expect(CH_COLON, '`:`')
        const index = this.takeInt()
        this.expect(CH_SEMICOLON, '`;`')
        if (index < 1n) this.fail('引用编号从 1 开始')
        // 编号指向序列化时第几个值。**不去校验它落在哪** —— 那要求我们复刻
        // PHP 序列化时的遍历与计数规则，而那套规则正是往返自检要兜住的坑
        // （doc/frontend/codecs.md「往返自检」）
        return { t: 'obj', cls: `${REF_MARK}${String.fromCharCode(tag)}:${index}`, v: [] }
      }

      default: {
        this.at -= 1
        this.fail(`没有 \`${String.fromCharCode(tag)}\` 这种 token`)
      }
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

class Writer {
  private buf = new Uint8Array(256)
  private len = 0

  private need(extra: number): void {
    if (this.len + extra <= this.buf.length) return
    let size = this.buf.length * 2
    while (size < this.len + extra) size *= 2
    const grown = new Uint8Array(size)
    grown.set(this.buf.subarray(0, this.len))
    this.buf = grown
  }

  /** 调用方要保证是 ASCII：token、数字、`__serialized` 这类。类名不算。 */
  ascii(text: string): void {
    this.need(text.length)
    for (let i = 0; i < text.length; i += 1) {
      this.buf[this.len] = text.charCodeAt(i)
      this.len += 1
    }
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

/**
 * float 重编成 PHP 会写出来的样子。
 *
 * PHP 7.1 起 `serialize_precision` 默认 -1，即 `php_gcvt` 的 mode 0：**最短往返
 * 表示**。这与 JS `toExponential()` 不带参数时给的有效数字完全一致（两边都是
 * "唯一确定这个 double 所需的最少位数"），所以数字本身直接取 JS 的。
 * 剩下的是 `php_gcvt` 的**排版规则**，它跟 JS `toString()` 不一样：
 *
 *   - ndigit 固定 17（mode 0 下 php_gcvt 自己设的）
 *   - 小数点位置 decpt 满足 `decpt < 0 ? decpt < -3 : decpt > 17` 时用指数形式。
 *     JS 的阈值是 1e21 / 1e-7，**两边不同**：`1e-5` PHP 写 `1.0E-5`，
 *     JS 写 `0.00001`；照搬 `String(v)` 会让这类值白白变成只读
 *   - 指数形式的小数部分至少一位（`1e100` → `1.0E+100` 而不是 `1E+100`），
 *     指数字符是 `E`、必带正负号、不补零
 *
 * **这个函数只服务"用户改出来的新值"。** 从输入解出来的 float 带着 `literal`，
 * 写回时走 `doubleText()` 原样吐回去，根本到不了这里 —— 因为**精度位数**是
 * 运行时可配的，同一个值在不同环境下是不同的字节，重建只能顾一头。
 * 见 types.ts 的 `literal` 一节。
 *
 * 排版规则已在真实 PHP 上交叉验证（`docker run --rm php:{5.6,7.4,8.3}-cli`）：
 *
 *   5.6（`serialize_precision=17`）  d:100;  d:0.10000000000000001;  d:1.0000000000000001E-5;  d:1.0E+17;
 *   7.4 / 8.3（`-1`）               d:100;  d:0.1;                  d:1.0E-5;                d:1.0E+17;
 *
 * 三个版本**排版一致**，只有位数不同：定点/指数的分界确实落在 `1e16`
 * （`d:10000000000000000;`）与 `1e17`（`d:1.0E+17;`）之间，指数形式的小数部分
 * 补足一位，指数字符 `E` 带正负号不补零。整数值的 float 写的是 `d:100;`
 * 而**不是** `d:100.0;` —— `smart_str_append_double` 的 `zero_frac` 是
 * `var_export` / `json_encode` 那条路径的行为，`serialize()` 不走它。
 */
/**
 * `literal` 还表示同一个值吗。
 *
 * 两件事一起做，都不能省：
 *
 *   - **值校验**：`literal` 是解码时记下的，用户改了值之后它就过时了。
 *     校验一道，「原样写回」就永远写不出错误的值。
 *   - **文法校验**：`literal` 会被**原样拼进输出**，所以它必须仍然是一个纯粹的
 *     浮点字面量。一个构造出来的 `1;s:4:"evil";` 会把额外的序列化数据注进去，
 *     而 `Node` 是可以由编辑路径重建的 —— 不能假设它只来自我们自己的解析器。
 */
function sameDouble(literal: string, value: number): boolean {
  if (literal === 'NAN') return Number.isNaN(value)
  if (literal === 'INF') return value === Number.POSITIVE_INFINITY
  if (literal === '-INF') return value === Number.NEGATIVE_INFINITY
  if (!DOUBLE_TEXT.test(literal)) return false
  // Object.is 而不是 `===`：`-0` 与 `0` 在 PHP 里是两串不同的字节
  return Object.is(Number(literal), value)
}

/** 写 `d:` 的内容。原样写回优先，理由见 `sameDouble` 与 types.ts 的 `literal` 一节。 */
function doubleText(node: { v: number; literal?: string }): string {
  if (node.literal !== undefined && sameDouble(node.literal, node.v)) return node.literal
  return phpDoubleText(node.v)
}

function phpDoubleText(value: number): string {
  if (Number.isNaN(value)) return 'NAN'
  if (value === Number.POSITIVE_INFINITY) return 'INF'
  if (value === Number.NEGATIVE_INFINITY) return '-INF'
  // `-0` 与 `0` 在 PHP 里是两串不同的字节，而 toExponential() 把符号丢了
  if (value === 0) return Object.is(value, -0) ? '-0' : '0'

  const sign = value < 0 ? '-' : ''
  const [mantissa, exponent] = Math.abs(value).toExponential().split('e')
  let digits = mantissa.replace('.', '')
  while (digits.length > 1 && digits.endsWith('0')) digits = digits.slice(0, -1)
  const decpt = Number(exponent) + 1

  if (decpt < 0 ? decpt < -3 : decpt > 17) {
    const power = decpt - 1
    const fraction = digits.length > 1 ? digits.slice(1) : '0'
    return `${sign}${digits[0]}.${fraction}E${power < 0 ? '-' : '+'}${Math.abs(power)}`
  }
  if (decpt <= 0) return `${sign}0.${'0'.repeat(-decpt)}${digits}`
  if (digits.length <= decpt) return `${sign}${digits}${'0'.repeat(decpt - digits.length)}`
  return `${sign}${digits.slice(0, decpt)}.${digits.slice(decpt)}`
}

function writeString(writer: Writer, raw: Uint8Array): void {
  writer.ascii(`s:${raw.length}:"`)
  writer.bytes(raw)
  writer.ascii('";')
}

function writeKey(writer: Writer, node: Node): void {
  if (node.t !== 'int' && node.t !== 'str' && node.t !== 'bytes') {
    throw new Error('PHP 数组的 key 只能是整数或字符串')
  }
  writeValue(writer, node, 0)
}

function writePairs(writer: Writer, pairs: Array<[Node, Node]>, depth: number): void {
  for (const [key, value] of pairs) {
    writeKey(writer, key)
    writeValue(writer, value, depth + 1)
  }
  writer.ascii('}')
}

/**
 * `obj` 的四种来源，靠类名上的标记区分（见 `ENUM_MARK` 那一段的理由）。
 *
 * 引用与 `C:` 在这里**主动抛错**，而不是编出点什么来凑：
 *
 *   - `R:` / `r:` 的编号指向序列化时的第几个值，取决于当时的遍历顺序。
 *     结构一改编号就指错，写回去是静默的数据错乱
 *   - `C:` 的内容是类自己写的字节，格式不自描述。我们能原样搬回去，但**不知道**
 *     改动之后它对那个类还成不成立（长度、校验、偏移都可能藏在里面），
 *     所以不能拿"字节能对上"去换一个"可以编辑"的承诺
 *
 * `checkRoundTrip` 会把抛出来的话当成只读理由显示，所以这两句是写给用户看的。
 */
function writeObject(writer: Writer, cls: string, pairs: Array<[Node, Node]>, depth: number): void {
  if (cls.startsWith(REF_MARK)) {
    throw new Error('R: / r: 引用的编号依赖序列化时的遍历顺序，重建出来会指错值')
  }
  // 顺序要紧：一个名叫 `C` 的 enum 解出来是 `C::X`，先判 `::` 才不会被当成 C:
  const enumSplit = cls.indexOf(ENUM_MARK)
  if (enumSplit >= 0) {
    if (pairs.length > 0) throw new Error('enum 只有类名与 case 名，没有属性')
    const payload = UTF8_ENCODER.encode(
      `${cls.slice(0, enumSplit)}:${cls.slice(enumSplit + ENUM_MARK.length)}`,
    )
    writer.ascii(`E:${payload.length}:"`)
    writer.bytes(payload)
    writer.ascii('";')
    return
  }
  if (cls.startsWith(CUSTOM_MARK)) {
    throw new Error('C: 的内容是类自己写的原始字节，格式不自描述，改了没法确认还成不成立')
  }

  const name = UTF8_ENCODER.encode(cls)
  writer.ascii(`O:${name.length}:"`)
  writer.bytes(name)
  writer.ascii(`":${pairs.length}:{`)
  writePairs(writer, pairs, depth)
}

function writeValue(writer: Writer, node: Node, depth: number): void {
  if (depth > MAX_DEPTH) throw new Error(`嵌套超过 ${MAX_DEPTH} 层`)

  switch (node.t) {
    case 'null':
      return writer.ascii('N;')
    case 'bool':
      return writer.ascii(node.v ? 'b:1;' : 'b:0;')
    case 'int':
      if (node.v < INT64_MIN || node.v > INT64_MAX) throw new Error('整数超出 PHP 的 int64')
      return writer.ascii(`i:${node.v};`)
    case 'float':
      return writer.ascii(`d:${doubleText(node)};`)
    case 'str':
      return writeString(writer, UTF8_ENCODER.encode(node.v))
    case 'bytes':
      return writeString(writer, node.v)
    case 'arr': {
      // 我们自己解不出 arr（PHP 数组一律是 map），但编辑之后可能送进来一个。
      // PHP 没有 list 这种类型，等价物就是下标连续的数组
      writer.ascii(`a:${node.v.length}:{`)
      writePairs(
        writer,
        node.v.map((item, index): [Node, Node] => [{ t: 'int', v: BigInt(index) }, item]),
        depth,
      )
      return
    }
    case 'map':
      writer.ascii(`a:${node.v.length}:{`)
      return writePairs(writer, node.v, depth)
    case 'obj':
      return writeObject(writer, node.cls, node.v, depth)
    case 'field':
    case 'ext':
      throw new Error('PHP serialize 里没有 protobuf 字段 / msgpack ext 这种东西')
    case 'seq':
    case 'call':
      // pickle 的 tuple / 构造调用。PHP 里没有对应物，而且**也不该去凑一个** ——
      // 这两个变体只可能来自只读解析器，它们的值本来就不该走到任何 encode 路径
      throw new Error('PHP serialize 里没有 tuple / 构造调用这种东西')
    case 'java':
      throw new Error('PHP serialize 里没有 Java class / handle / blockdata 这种东西')
  }
}

function encode(node: Node): Uint8Array {
  const writer = new Writer()
  writeValue(writer, node, 0)
  return writer.take()
}

/**
 * 注册进 `RENDERER_CODECS.phpSerialize` 的那一项，挂载写在 `index.ts` 的表里。
 *
 * **片 3（msgpack）与片 4（protobuf）照这个形状各写一个文件**：模块自己只
 * 依赖 `./types`（外加类型 `RendererCodec`，`import type` 会被完全擦掉，
 * 所以不构成循环依赖），对外只导出一个 codec 常量。注册集中在表上而不是靠
 * 各模块的副作用自注册 —— 后者会引入"没人 import 它就没注册"的启动顺序问题。
 */
export const phpSerializeCodec: RendererCodec = { decode, encode }
