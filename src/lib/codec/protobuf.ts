import type { RendererCodec } from './index'
import type { Node } from './types'

/**
 * Protobuf 的 wire-format 探查器：**无 schema** 的解析与重编码。
 *
 * ## 为什么自写（doc/frontend/codec-formats.md「依赖：全部自写」）
 *
 * `protobufjs` 是 schema 驱动的，而我们没有 `.proto`。无 schema 时能解出来的
 * 只有字段号与 wire type —— 需要的是一个**探查器**，跟"按消息定义解出字段名"
 * 的库形状根本不匹配。`.proto` 导入是独立特性（文件选择、schema 存储、类型解析
 * 一整套），这里不做。
 *
 * ## 格式出处
 *
 * 独立编写，未参考也未移植任何现成实现（CLAUDE.md 的来源约束）。对照的是
 * Protocol Buffers 官方文档的 **Encoding** 一篇：
 *
 *   - "Base 128 Varints"：每字节低 7 位是数据、最高位是继续位，**低位组在前**
 *   - "Message Structure"：一条记录是 `tag + payload`，
 *     `tag = (field_number << 3) | wire_type`
 *   - 同一篇的 wire type 表：0 VARINT、1 I64、2 LEN、5 I32，
 *     另有标着 deprecated 的 3 SGROUP、4 EGROUP
 *   - "More Integer Types"：I32 / I64 是**小端**（跟 msgpack 的全大端正好相反），
 *     且同一个 wire type 对应多种 `.proto` 类型 —— 见下面"wire 1 / 5"那一节
 *   - "Repeated Elements" 与 "Packed Repeated Fields"：同一个字段号可以出现多次，
 *     packed 的写法是把一串值塞进**一条** LEN 记录
 *   - "Field Orders"：字段顺序不是语义的一部分，但**是字节的一部分**，
 *     所以这里原样保留，不排序也不合并
 *
 * ## 结构映射：message 是一串 field，所以是 `arr` 不是 `map`
 *
 * message → `{ t: 'arr', v: Node[] }`，每一项是 `{ t: 'field', no, wire, v }`。
 * 用 `map` 表达不了 repeated（同一个字段号可以出现多次），顺序也会丢 ——
 * 而顺序是往返要的字节的一部分。
 *
 * ## 呈现是猜测，必须标出来（doc/frontend/codec-formats.md「Protobuf」）
 *
 * wire type 2 可能是 string / bytes / 嵌套 message / packed repeated，无 schema
 * 时**只能猜**：递归试解为嵌套 message → 试 UTF-8 → 退回字节。
 *
 * 关键性质：**猜测只影响展示，不影响往返。** 不管一段 LEN 记录被猜成 message
 * 还是 string，重编回去都是同一串字节 —— 猜成 message 时长度前缀跟着内部重编
 * 的实际长度走（所以内部有非最短编码时，整条记录的字节也跟着对不上，
 * 自检如实判只读），猜成 string 时 UTF-8 编解码在合法序列上是双射。
 *
 * 标注（`~msg` / `~str` / `~bytes`）落在 `render.ts` 而不是存进 `Node`：它由
 * 子节点的类型**推导**得出（`arr` 只可能来自试解成功的嵌套 message，`str` 只
 * 可能来自试 UTF-8 成功，`bytes` 是兜底），存一份反而会跟数据不同步。
 * "protobuf 无 schema 时怎么呈现本身就是展示决策"（doc/backend/codec.md「归属切分」），
 * 那这个决策就该住在展示层。
 *
 * ## wire 1 / 5 收成**整数**，不是 float
 *
 * 填 `float`（fixed32 → `bits: 32`、fixed64 → `bits: 64`）会把一个**猜测**塞进
 * 本该无损的那一层，而且会让往返正确性依赖一条实现定义的行为。
 *
 *   - wire 1 是 `fixed64` / `sfixed64` / `double` 共用的，wire 5 是 `fixed32` /
 *     `sfixed32` / `float` 共用的。无 schema 时选浮点是**猜**，收成原始位不是。
 *     wire 2 那边不猜就什么都显示不了，这里不猜也能无损表达 —— 性质不同
 *   - **小的负 `sfixed32` 全都落在 float32 的 NaN 位型上**：-1 是 `ff ff ff ff`，
 *     -2 是 `fe ff ff ff`，指数全 1、尾数非零，逐个实测都是 NaN；`sfixed64`
 *     同理。而 ECMAScript 对 `DataView.setFloat32/64` 写 NaN 只承诺"某个实现
 *     定义的位型"，不承诺保位。本机 V8 实测保住了，但本应用的 webview 在
 *     macOS / Linux 上是 JSC，而 JSC 的 NaN-boxing 会把非规范 NaN purify 掉。
 *     也就是说：**把 `sfixed32 = -1` 收成 float 节点，在半数平台上会让一个
 *     再普通不过的值被自检误判成只读**，而 `Node` 里存的那个 double 也已经
 *     丢了原来的位型，想复原都复原不回来
 *   - 两种读法用户都该看见 —— 那是展示层的事：`render.ts` 拿原始位按 IEEE 754
 *     重解释一次，显示成 `i64 1065353216 (float 1)`。信息比 float 节点更多，
 *     而 `Node` 保持无损
 *
 * varint 的负数同理：wire 0 上 `int64 -1`、`uint64 2^64-1`、`sint64` 的 zigzag
 * 值在字节上完全一样，**无 schema 时分不出来**。所以这里一律收成
 * `[0, 2^64-1]` 的原始无符号整数，不做任何符号还原；补码读法同样交给 `render.ts`
 * 显示（zigzag 不显示：它对每个值都有一种读法，`1` 会变成 `-1`，那是噪声不是信息）。
 *
 * ## group（wire 3 / 4）：忠实解析，不是默默出错
 *
 * 三条理由，选"解"而不是"拒"：
 *
 *   1. 它仍然在 Encoding 文档里，只是标着 deprecated。proto2 的老数据、
 *      `MessageSet` 这类格式里是真实存在的。拒掉等于把一份解得开的数据
 *      报成"不是 protobuf"，那才是更坏的结局
 *   2. group **没有长度前缀**，所以往返比 LEN 记录还干净：写回去就是
 *      开始 tag + 字段 + 结束 tag，不存在"内部重编后长度变了"这回事
 *   3. 它自带一条强约束 —— 结束 tag 的字段号必须与开始的对上，且不能跨越
 *      LEN 记录的边界。**不平衡就抛错**，于是它顺带成了嵌套 message 猜测的
 *      一道过滤：一串文本里冒出来的 EGROUP（例如 `"hello"` 的 `0x6c`）没有
 *      对应的开始标记，那段就判定"不是 message"，退给 UTF-8 那一级
 *
 * 没有留默默出错的口子：wire 4 只能作为终止符出现，落单的、字段号对不上的、
 * 到末尾还没闭合的，三种都抛错；wire 6 / 7 没有定义，也抛错。
 *
 * ## 严格是刚性要求
 *
 * 解完必须消耗掉全部输入 —— 对 protobuf 这条是构造保证的：message 就是一串
 * 记录，根解析器一路读到末尾，所以"尾部多余字节"表现为**那段字节自己得是
 * 一条合法记录**，不是的话在那里就抛错了。**decode 抛错 = 不是这个格式**，
 * 不要返回"部分解出来"的结构。字段号范围、长度前缀不越界、嵌套深度上限都在
 * 下面各自的位置卡住（炸栈抛的 `RangeError` 混在"解不开"里看不出发生了什么）。
 *
 * protobuf 进不了自动识别的候选集（`lib/codecs.ts` 的 `detectable: false`，
 * 理由见 doc/frontend/codecs.md「识别的三个维度」：无魔数，任意字节都可能是合法
 * protobuf），但
 * `checkRoundTrip` 与将来的连接级配置都按"解不开就抛"来用它。
 */

/* ── wire type 与 tag。数值出自 Encoding 文档 ───────────── */

const WIRE_VARINT = 0
const WIRE_FIXED64 = 1
const WIRE_LEN = 2
const WIRE_SGROUP = 3
const WIRE_EGROUP = 4
const WIRE_FIXED32 = 5

/** tag 的低三位是 wire type，其余右移三位是字段号。 */
const WIRE_MASK = 0x07
const TAG_SHIFT = 3
/** 同一个位移量的 bigint 那一份，拼 tag 时用。 */
const TAG_SHIFT_BIG = 3n

/**
 * 字段号的合法范围是 1 到 2^29-1。
 *
 * **0 不是"没用过的号"而是非法**，这一卡挡掉的是最常见的一类误判：任何以
 * `00`–`07` 开头的字节串都会当场被判"不是 message"，退给 UTF-8 / 字节那两级。
 *
 * 19000–19999 是 protobuf 留给自己实现的号段，**这里不卡**：那是 `.proto`
 * 声明层面的限制，wire format 本身对它没有说法，卡了只会把非标准编码器写出来
 * 的真实数据判成"不是 protobuf"。
 */
const MIN_FIELD_NO = 1
const MAX_FIELD_NO = 536870911

/** tag 最宽就是 `(2^29-1) << 3 | 7` = 2^32-1，超了必然是字段号越界。 */
const TAG_MAX = 0xffffffffn

const U32_MAX = 0xffffffffn
const UINT64_MAX = 18446744073709551615n

/** varint 是 7 位一组，64 位的值最多 10 组。 */
const MAX_VARINT_BYTES = 10

/**
 * 嵌套上限，取 **100** —— 跟 php.ts / msgpack.ts 的 256 不同，是有出处的：
 * protobuf 官方实现的默认递归上限就是 100（`CodedInputStream` 的
 * recursion limit），真实数据不会更深。
 *
 * 第二个理由是这里独有的：LEN 记录要**试解**成嵌套 message，同一段字节在每一
 * 层都会被重新扫一遍，总开销是 O(输入长度 × 深度)。上限直接是这个乘数，
 * 8 MB 的输入乘 256 层就够卡住渲染线程一会儿了。
 */
const MAX_DEPTH = 100

/**
 * `fatal: true`：解不开就说明"这段不是文本"，该退回字节。默认的宽松解码会把
 * 非法序列换成一串 U+FFFD —— 那是篡改数据，而且不可逆。
 *
 * `ignoreBOM: true`：默认会**吃掉**开头的 U+FEFF，解出来的串比原字节少三个，
 * 重编回去字节就对不上了。
 */
const UTF8_DECODER = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true })
const UTF8_ENCODER = new TextEncoder()

/* ── 解析 ─────────────────────────────────────────────── */

/**
 * 一个 `Reader` 只负责**一段 message 区域**：整段输入，或者一条 LEN 记录的
 * payload。嵌套 message 的试解另起一个 `Reader`，所以 group 天然不会跨越
 * LEN 记录的边界 —— 那正是 wire format 的要求。
 *
 * 附带一个好性质：试解失败的错都被 `tryMessage` 吞掉了，**能冒到用户面前的
 * 错一定来自根 `Reader`**，所以错误信息里的偏移一律是整段输入里的绝对偏移。
 */
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
    throw new Error(`不是 Protobuf 数据（偏移 ${this.at}）：${message}`)
  }

  private need(count: number): void {
    if (this.at + count > this.src.length) {
      this.fail(`还差 ${this.at + count - this.src.length} 个字节`)
    }
  }

  private atEnd(): boolean {
    return this.at >= this.src.length
  }

  private remaining(): number {
    return this.src.length - this.at
  }

  /** 见文件头"严格"一节：这里是构造保证之外的一道廉价保险。 */
  expectEnd(): void {
    if (this.at !== this.src.length) {
      this.fail(`末尾还剩 ${this.remaining()} 个字节没有解释`)
    }
  }

  private u8(): number {
    this.need(1)
    const value = this.src[this.at]
    this.at += 1
    return value
  }

  /**
   * Base 128 varint：每字节低 7 位是数据、最高位是继续位，**低位组在前**。
   *
   * 全程 bigint：wire 0 装得下 `uint64`，塞进 number 会静默丢精度，而
   * `Node.int` 只有 bigint 一种形态。
   */
  private varint(): bigint {
    let value = 0n
    let shift = 0n
    for (let i = 0; i < MAX_VARINT_BYTES; i += 1) {
      const byte = this.u8()
      value |= BigInt(byte & 0x7f) << shift
      if ((byte & 0x80) === 0) {
        // 第 10 个字节能带进第 64 位以上的位，那样的值 64 位装不下
        if (value > UINT64_MAX) this.fail('varint 的值超出 64 位')
        return value
      }
      shift += 7n
    }
    this.fail(`varint 超过 ${MAX_VARINT_BYTES} 个字节`)
  }

  /** I32 / I64 是**小端**（`littleEndian = true`），跟 msgpack 的全大端相反。 */
  private fixed(byteCount: number): bigint {
    this.need(byteCount)
    const value =
      byteCount === 4
        ? BigInt(this.view.getUint32(this.at, true))
        : this.view.getBigUint64(this.at, true)
    this.at += byteCount
    return value
  }

  /**
   * 一段 message 区域里的所有记录。
   *
   * `closing` 非空表示这是一个 group 的内部：必须以字段号相同的 EGROUP 收尾，
   * 读到末尾还没收到就是不平衡。`closing` 为空（根 message 或 LEN payload）时
   * 读到末尾就正常结束，而落单的 EGROUP 是错误。
   */
  readMessage(depth: number, closing: number | null): Node {
    if (depth > MAX_DEPTH) this.fail(`嵌套超过 ${MAX_DEPTH} 层`)

    const fields: Node[] = []
    for (;;) {
      if (this.atEnd()) {
        if (closing !== null) this.fail(`字段 #${closing} 的 group 没有结束标记`)
        return { t: 'arr', v: fields }
      }

      const tagAt = this.at
      const raw = this.varint()
      if (raw > TAG_MAX) {
        this.at = tagAt
        this.fail('tag 超出 32 位，字段号不可能这么大')
      }
      const tag = Number(raw)
      // `>>>` 而不是 `>>`：tag 顶到 2^32-1 时，有符号右移给出的是负数
      const no = tag >>> TAG_SHIFT
      const wire = tag & WIRE_MASK

      if (wire === WIRE_EGROUP) {
        if (closing === null) {
          this.at = tagAt
          this.fail(`#${no} 的 group 结束标记没有对应的开始标记`)
        }
        if (no !== closing) {
          this.at = tagAt
          this.fail(`group 是 #${closing} 开的，结束标记却写着 #${no}`)
        }
        return { t: 'arr', v: fields }
      }

      if (no < MIN_FIELD_NO || no > MAX_FIELD_NO) {
        this.at = tagAt
        this.fail(`字段号 ${no} 不在 ${MIN_FIELD_NO}..${MAX_FIELD_NO} 里`)
      }

      fields.push(this.readField(no, wire, depth, tagAt))
    }
  }

  private readField(no: number, wire: number, depth: number, tagAt: number): Node {
    switch (wire) {
      // 原始无符号值，不还原符号也不还原 zigzag —— 无 schema 时分不出来，见文件头
      case WIRE_VARINT:
        return { t: 'field', no, wire, v: { t: 'int', v: this.varint() } }

      // 定宽的**位**，不是浮点。另一种读法由 render.ts 重解释出来，见文件头
      case WIRE_FIXED64:
        return { t: 'field', no, wire, v: { t: 'int', v: this.fixed(8) } }
      case WIRE_FIXED32:
        return { t: 'field', no, wire, v: { t: 'int', v: this.fixed(4) } }

      case WIRE_LEN: {
        const lenAt = this.at
        const len = this.varint()
        // 长度前缀不能越界。剩余字节数在**回退偏移之前**算，不然报的数会把
        // 长度前缀自己那几个字节也算进去
        const left = this.remaining()
        if (len > BigInt(left)) {
          this.at = lenAt
          this.fail(`长度前缀说有 ${len} 个字节，只剩 ${left} 个`)
        }
        const payload = this.src.subarray(this.at, this.at + Number(len))
        this.at += Number(len)
        return { t: 'field', no, wire, v: guessPayload(payload, depth) }
      }

      // group 没有长度前缀，内容就跟在开始 tag 后面，直到字段号相同的 EGROUP
      case WIRE_SGROUP:
        return { t: 'field', no, wire, v: this.readMessage(depth + 1, no) }

      default:
        this.at = tagAt
        this.fail(`wire type ${wire} 没有定义`)
    }
  }
}

/**
 * LEN 记录的 payload 到底是什么：**猜**，顺序见 doc/frontend/codec-formats.md「Protobuf」。
 *
 * 三级都不影响往返（见文件头），所以这里可以放心按"哪个更可能有用"排：
 * 嵌套 message → UTF-8 文本 → 原始字节。
 */
function guessPayload(payload: Uint8Array, depth: number): Node {
  /* 空 payload 三种猜法都成立，也都没有信息量。选空串是因为另外两个更差：
     空 message 会平白多出一层嵌套，而 `bytes(0)` 比 `""` 更不像真实数据里
     那个最常见的来源 —— 一个空 string 字段。 */
  if (payload.length === 0) return { t: 'str', v: '' }

  const nested = tryMessage(payload, depth + 1)
  if (nested) return nested

  if (looksLikeText(payload)) {
    try {
      return { t: 'str', v: UTF8_DECODER.decode(payload) }
    } catch {
      // 不是合法 UTF-8，退回字节
    }
  }

  // slice 而不是 subarray：不能让一个两字节的字段吊着整段 8 MB 输入的引用
  return { t: 'bytes', v: payload.slice() }
}

/**
 * 试着把这段 payload 解成一个完整的嵌套 message。解不开 / 没解完就是"不是"。
 *
 * 深度超限也是从这里被吞掉的（`readMessage` 抛错 → 这里 `null`）：于是超深的
 * 输入**降级**成十六进制字节，而不是整份解不开。它仍然一字节不差地往返 ——
 * `bytes` 节点原样写回去就是原来那段 payload。
 */
function tryMessage(payload: Uint8Array, depth: number): Node | null {
  const reader = new Reader(payload)
  try {
    const node = reader.readMessage(depth, null)
    reader.expectEnd()
    return node
  } catch {
    return null
  }
}

/**
 * 合法 UTF-8 还不够，还要"看着像文本"。
 *
 * C0 控制字符（`\t` `\n` `\r` 除外）在真实的 string 字段里几乎不出现，在
 * packed repeated 与短二进制里却到处都是：`01 02 03` 是合法 UTF-8，猜成
 * 字符串就是三个不可见字符，猜成字节才看得见。
 *
 * 按字节判与按码点判在 C0 这一段完全等价：UTF-8 的多字节序列每个字节都 ≥ 0x80。
 */
function looksLikeText(payload: Uint8Array): boolean {
  for (let i = 0; i < payload.length; i += 1) {
    const byte = payload[i]
    if (byte < 0x20 && byte !== 0x09 && byte !== 0x0a && byte !== 0x0d) return false
  }
  return true
}

/**
 * 根 message 一路读到输入末尾。**空输入解出一个空 message** —— 那是合法编码，
 * 不是"解不开"。
 */
function decode(bytes: Uint8Array): Node {
  const reader = new Reader(bytes)
  const node = reader.readMessage(0, null)
  reader.expectEnd()
  return node
}

/* ── 重编码 ───────────────────────────────────────────── */

/**
 * varint 一律写**最短形式**。
 *
 * wire format 允许非最短编码（`80 00` 也是 0），而真实的 protobuf 库都只写最短
 * 形式。所以非最短的输入重编后字节对不上 → 往返自检判只读。
 *
 * **这是正确行为，不要为它开特例。** 跟 PHP 的 float 字面量不是一回事：那里是
 * 标准 PHP 自己就产出多种写法，属于常态，所以 `Node.float.literal` 把表示的
 * 选择记了下来；非最短 varint 是罕见的非标准输出，为它留后门等于放弃自检。
 */
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

  u8(value: number): void {
    this.need(1)
    this.buf[this.len] = value
    this.len += 1
  }

  varint(value: bigint): void {
    let rest = value
    while (rest >= 0x80n) {
      this.u8(Number(rest & 0x7fn) | 0x80)
      rest >>= 7n
    }
    this.u8(Number(rest))
  }

  /** I32 / I64 是小端，低位字节先写。 */
  fixed(value: bigint, byteCount: number): void {
    this.need(byteCount)
    let rest = value
    for (let i = 0; i < byteCount; i += 1) {
      this.buf[this.len + i] = Number(rest & 0xffn)
      rest >>= 8n
    }
    this.len += byteCount
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
 * `wire` 决定这一格该写什么，所以取值必须对得上。这些错都是**编辑路径**才
 * 造得出来的（我们自己解出来的树天然自洽），`checkRoundTrip` 会把话原样显示
 * 成只读理由，所以要写成给用户看的句子。
 */
function intOf(node: Node, max: bigint, what: string): bigint {
  if (node.t !== 'int') throw new Error(`${what}装的是整数，不是 ${node.t}`)
  if (node.v < 0n || node.v > max) throw new Error(`${what}的值要在 0..${max} 之间`)
  return node.v
}

/** LEN 记录的内容：三种猜法各自怎么写回去，写出来的都是原来那段 payload。 */
function payloadBytes(node: Node, depth: number): Uint8Array {
  switch (node.t) {
    // 嵌套 message：长度前缀跟着**内部重编的实际长度**走，不是记下来的原长度
    case 'arr':
      return messageBytes(node.v, depth + 1)
    case 'str':
      return UTF8_ENCODER.encode(node.v)
    case 'bytes':
      return node.v
    default:
      throw new Error(`wire type 2 装的是嵌套 message / 字符串 / 字节，不是 ${node.t}`)
  }
}

function writeField(writer: Writer, node: Node, depth: number): void {
  if (node.t !== 'field') {
    throw new Error(`Protobuf 的 message 每一项都得是字段，这里是 ${node.t}`)
  }
  if (!Number.isInteger(node.no) || node.no < MIN_FIELD_NO || node.no > MAX_FIELD_NO) {
    throw new Error(`字段号 ${node.no} 不在 ${MIN_FIELD_NO}..${MAX_FIELD_NO} 里`)
  }

  const tag = (BigInt(node.no) << TAG_SHIFT_BIG) | BigInt(node.wire)

  switch (node.wire) {
    case WIRE_VARINT:
      writer.varint(tag)
      return writer.varint(intOf(node.v, UINT64_MAX, `字段 #${node.no}（varint）`))
    case WIRE_FIXED64:
      writer.varint(tag)
      return writer.fixed(intOf(node.v, UINT64_MAX, `字段 #${node.no}（fixed64）`), 8)
    case WIRE_FIXED32:
      writer.varint(tag)
      return writer.fixed(intOf(node.v, U32_MAX, `字段 #${node.no}（fixed32）`), 4)
    case WIRE_LEN: {
      const payload = payloadBytes(node.v, depth)
      writer.varint(tag)
      writer.varint(BigInt(payload.length))
      return writer.bytes(payload)
    }
    case WIRE_SGROUP: {
      if (node.v.t !== 'arr') {
        throw new Error(`字段 #${node.no}（group）装的是一串字段，不是 ${node.v.t}`)
      }
      writer.varint(tag)
      writeMessage(writer, node.v.v, depth + 1)
      // 结束 tag 的字段号必须与开始的一致，wire type 是 4
      return writer.varint((BigInt(node.no) << TAG_SHIFT_BIG) | BigInt(WIRE_EGROUP))
    }
    default:
      // 4 只能当 group 的终止符（由 WIRE_SGROUP 那一支写出来），6 / 7 没有定义
      throw new Error(`字段 #${node.no} 的 wire type ${node.wire} 不能单独出现`)
  }
}

function writeMessage(writer: Writer, fields: Node[], depth: number): void {
  if (depth > MAX_DEPTH) throw new Error(`嵌套超过 ${MAX_DEPTH} 层`)
  for (const field of fields) writeField(writer, field, depth)
}

function messageBytes(fields: Node[], depth: number): Uint8Array {
  const writer = new Writer()
  writeMessage(writer, fields, depth)
  return writer.take()
}

function encode(node: Node): Uint8Array {
  if (node.t !== 'arr') {
    throw new Error(`Protobuf 的顶层是一个 message（字段的数组），不是 ${node.t}`)
  }
  return messageBytes(node.v, 0)
}

/**
 * 注册进 `RENDERER_CODECS.protobuf` 的那一项，挂载写在 `index.ts` 的表里。
 *
 * 注意它进不了自动识别的候选集（`lib/codecs.ts` 的 `detectable: false`）：
 * 无魔数，任意字节都可能是合法 protobuf，进了候选就会把一切都识别成它。
 * "能不能跑"与"能不能自动识别"是两个维度，见 doc/frontend/codecs.md「识别的三个维度」。
 */
export const protobufCodec: RendererCodec = { decode, encode }
