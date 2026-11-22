import type {
  JavaClassData,
  JavaClassDataLayout,
  JavaClassLayer,
  JavaContent,
  JavaValue,
  Node,
} from './types'

/**
 * `Node` → 展示文本。
 *
 * **不是 JSON，而且刻意不像 JSON。** `Node` 之所以存在，就是因为 JSON 装不下
 * int64 精度、二进制字段、非字符串 map key 与类名（见 `types.ts` 的注释）；
 * 渲染成一份"看起来像 JSON 的东西"等于把这些区别又抹掉一次。所以：
 *
 *   - 用 `=>` 而不是 `:` 分隔键值 —— 一眼就能看出这不是 JSON，
 *     而且 key 位置上摆一个非字符串是正常的
 *   - JSON 装得下的类型不加标注（`"str"` / `true` / `null`），
 *     JSON 装不下的一律加：`i64` / `f32` / `f64` / `bytes(n)` / `ext(code)` / 类名
 *   - 不加逗号：这份文本不该被复制去当 JSON 用
 *
 * 产物进 `<pre class="term">`，等宽字体是数据的字体，这里全是数据。
 */

const INDENT = '  '

/** 每一层容器最多展开多少项。超了要**说出来**，不能默默截断。 */
const MAX_ITEMS = 2000

/** 字节最多列多少个，其余用 `…`；`bytes(n)` 里的 n 始终是真实长度。 */
const MAX_BYTES = 64

/**
 * 嵌套上限。除了防止一屏塞不下，它还兼作**环路保护**：
 * PHP 的 `R:` / `r:` 是引用，解析器如果把它还原成共享子树，这棵树就不再是树了。
 */
const MAX_DEPTH = 32

export function renderNode(node: Node): string {
  return render(node, 0)
}

function render(node: Node, depth: number): string {
  switch (node.t) {
    case 'null':
      return 'null'
    case 'bool':
      return node.v ? 'true' : 'false'
    /* 标注不是装饰：JSON 里 1 和 1.0 和 2^53+1 长得一样，这里必须分得开 */
    case 'int':
      return `i64 ${node.v}`
    case 'float':
      return `f${node.bits} ${renderFloat(node.v)}`
    case 'str':
      return JSON.stringify(node.v)
    case 'bytes':
      return renderBytes(node.v)
    case 'ext':
      return `ext(${node.code}) ${renderBytes(node.v)}`
    case 'field':
      // protobuf 无 schema 时只有字段号与 wire type，把它们摆出来而不是编个名字
      return renderField(node, depth)
    case 'arr':
      return block('[', ']', node.v.length, (i) => render(node.v[i], depth + 1), depth)
    case 'map':
      return block('{', '}', node.v.length, (i) => renderPair(node.v[i], depth + 1), depth)
    case 'obj':
      // 类名单独摆在前面：它是 PHP 的 O: / E: 解出来的事实，不是我们编的标签
      return `${node.cls} ${block('{', '}', node.v.length, (i) => renderPair(node.v[i], depth + 1), depth)}`
    case 'seq':
      // 圆括号 + 类名：`[…]` 是 list、`{…}` 是 dict 都已经占掉了，而 tuple / set /
      // frozenset 是三个**不同的类型**，挤成一样等于把 Node 保住的区别又抹掉
      return `${node.cls} ${block('(', ')', node.v.length, (i) => render(node.v[i], depth + 1), depth)}`
    case 'call':
      return renderCall(node, depth)
    case 'java':
      return renderJava(node.v, depth)
  }
}

function renderPair(entry: [Node, Node], depth: number): string {
  return `${render(entry[0], depth)} => ${render(entry[1], depth)}`
}

/* ── pickle：调用点就是那个危险的东西，所以照实写成调用 ── */

/**
 * `call` 的呈现。**用函数调用的语法本身当标注**，不另加警示前缀。
 *
 * 这是本功能的安全价值所在（doc/backend/codec.md「Readonly 安全边界」）：一个 `posix.system("curl … | sh")` 摆在
 * 屏幕上，用户一眼就懂"用 Python 加载这个值会发生什么"。加个 `⚠` 之类反而更糟
 * —— 绝大多数 `REDUCE` 是完全正常的（`collections.OrderedDict`、
 * `datetime.datetime`），逐个打危险标签就是狼来了，真正该被看见的那个反而淹了。
 *
 * 裸引用（`v === null`）与调用是**两种字节**，只有后者会执行，所以这里也分开写：
 * `datetime.datetime` 是一个引用，`datetime.datetime(…)` 是一次调用。
 *
 * `state` / `data` 跟在后面：它们是构造之后补上的东西（`__setstate__` 的状态、
 * `APPENDS` 塞进去的元素），漏掉等于少显示一半内容。
 */
function renderCall(node: Extract<Node, { t: 'call' }>, depth: number): string {
  const args = node.v
  const parts: string[] = [
    args === null
      ? node.fn
      : `${node.fn} ${block('(', ')', args.length, (i) => render(args[i], depth + 1), depth)}`,
  ]
  if (node.state !== undefined) parts.push(`state ${render(node.state, depth)}`)
  if (node.data !== undefined) parts.push(`items ${render(node.data, depth)}`)
  return parts.join(' ')
}

/* ── Java serialization：只呈现 wire 里能证明的事实 ─── */

function renderJava(node: JavaValue, depth: number): string {
  switch (node.kind) {
    case 'object':
      return `java object ${node.className} ${block(
        '{',
        '}',
        node.classes.length,
        (i) => renderJavaClassData(node.classes[i], depth + 1),
        depth,
      )}`
    case 'array':
      return `java array ${node.className} ${block(
        '[',
        ']',
        node.values.length,
        (i) => render(node.values[i], depth + 1),
        depth,
      )}`
    case 'primitiveArray':
      return renderJavaPrimitiveArray(node, depth)
    case 'enum':
      return `java enum ${node.className}.${render(node.name, depth)}`
    case 'class':
      return `java Class ${renderJavaClasses(node.classes, depth)}`
    case 'classDesc':
      return `java classDesc ${renderJavaClasses(node.classes, depth)}`
    case 'reference':
      // handle 会在 TC_RESET 后从 0x7e0000 重用，epoch 不显示就会把两个目标混成一个。
      return `&java:${node.epoch}:0x${node.handle.toString(16)} -> ${node.target}`
    case 'stream':
      return `java stream ${renderJavaContents(node.contents, depth)}`
    case 'exception':
      return `java write exception ${render(node.value, depth)}`
  }
}

function renderJavaClassData(data: JavaClassData, depth: number): string {
  const layer = data.descriptor
  const annotations =
    layer.annotations.length > 0
      ? ` class-annotation ${renderJavaContents(layer.annotations, depth + 1)}`
      : ''
  const alternatives = data.alternatives
  const body =
    alternatives && alternatives.length > 1
      ? `ambiguous-classdata ${block(
          '[',
          ']',
          alternatives.length,
          (index) =>
            `alternative ${index + 1} ${renderJavaClassLayout(alternatives[index], depth + 1)}`,
          depth,
        )}`
      : renderJavaClassLayout(data, depth)
  return `${renderJavaClassHeader(layer)} ${body}${annotations}`
}

function renderJavaClassLayout(data: JavaClassDataLayout, depth: number): string {
  const count = data.fields.length + data.custom.length
  return block(
    '{',
    '}',
    count,
    (position) => {
      const boundary = data.defaultFieldBoundary
      if (boundary === null) return `custom ${renderJavaContent(data.custom[position], depth + 1)}`
      if (position < boundary) {
        return `custom ${renderJavaContent(data.custom[position], depth + 1)}`
      }
      if (position < boundary + data.fields.length) {
        return renderJavaField(data.fields[position - boundary], depth + 1)
      }
      return `custom ${renderJavaContent(data.custom[position - data.fields.length], depth + 1)}`
    },
    depth,
  )
}

function renderJavaField(field: JavaClassData['fields'][number], depth: number): string {
  const descriptor = field.descriptor.className
    ? `${field.descriptor.type} ${field.descriptor.className}`
    : field.descriptor.type
  return `${descriptor} ${field.descriptor.name} => ${render(field.value, depth)}`
}

function renderJavaClasses(classes: JavaClassLayer[], depth: number): string {
  return block(
    '{',
    '}',
    classes.length,
    (index) => renderJavaClassLayer(classes[index], depth + 1),
    depth,
  )
}

function renderJavaClassLayer(layer: JavaClassLayer, depth: number): string {
  const extraCount = Number(layer.annotations.length > 0)
  return `${renderJavaClassHeader(layer)} ${block(
    '{',
    '}',
    layer.fields.length + extraCount,
    (index) => {
      if (index < layer.fields.length) {
        const field = layer.fields[index]
        const descriptor = field.className ? `${field.type} ${field.className}` : field.type
        return `field ${descriptor} ${field.name}`
      }
      return `class-annotation ${renderJavaContents(layer.annotations, depth + 1)}`
    },
    depth,
  )}`
}

function renderJavaClassHeader(layer: JavaClassLayer): string {
  if (layer.kind === 'proxy') {
    return `proxy implements ${layer.interfaces.length > 0 ? layer.interfaces.join(', ') : '(none)'}`
  }
  const suid = layer.serialVersionUid === null ? '' : ` suid=${layer.serialVersionUid}`
  const flags = layer.flags === null ? '' : ` flags=${renderJavaFlags(layer.flags)}`
  return `${layer.name}${suid}${flags}`
}

function renderJavaFlags(flags: number): string {
  const names: string[] = []
  if ((flags & 0x01) !== 0) names.push('SC_WRITE_METHOD')
  if ((flags & 0x02) !== 0) names.push('SC_SERIALIZABLE')
  if ((flags & 0x04) !== 0) names.push('SC_EXTERNALIZABLE')
  if ((flags & 0x08) !== 0) names.push('SC_BLOCK_DATA')
  if ((flags & 0x10) !== 0) names.push('SC_ENUM')
  const hex = `0x${flags.toString(16).padStart(2, '0')}`
  return names.length > 0 ? `${hex} ${names.join('|')}` : hex
}

function renderJavaContents(contents: JavaContent[], depth: number): string {
  return block(
    '(',
    ')',
    contents.length,
    (index) => renderJavaContent(contents[index], depth + 1),
    depth,
  )
}

function renderJavaContent(content: JavaContent, depth: number): string {
  if (content.kind === 'block') return `blockdata ${renderBytes(content.value)}`
  if (content.kind === 'reset') return `reset epoch=${content.epoch}`
  return render(content.value, depth)
}

function renderJavaPrimitiveArray(
  node: Extract<JavaValue, { kind: 'primitiveArray' }>,
  depth: number,
): string {
  // byte[] 的常见用途就是二进制 payload，列成几千个 i8 只会降低可读性。
  if (node.component === 'B') return `java array ${node.className} ${renderBytes(node.bytes)}`

  const view = new DataView(node.bytes.buffer, node.bytes.byteOffset, node.bytes.byteLength)
  return `java array ${node.className} ${block(
    '[',
    ']',
    node.count,
    (index) => renderJavaPrimitive(view, node.component, index),
    depth,
  )}`
}

function renderJavaPrimitive(view: DataView, component: string, index: number): string {
  switch (component) {
    case 'Z':
      return view.getUint8(index) === 0 ? 'false' : 'true'
    case 'C':
      return `char ${JSON.stringify(String.fromCharCode(view.getUint16(index * 2)))}`
    case 'S':
      return `i16 ${view.getInt16(index * 2)}`
    case 'I':
      return `i32 ${view.getInt32(index * 4)}`
    case 'J':
      return `i64 ${view.getBigInt64(index * 8)}`
    case 'F':
      return `f32 ${renderFloat(view.getFloat32(index * 4))}`
    case 'D':
      return `f64 ${renderFloat(view.getFloat64(index * 8))}`
    default:
      return `<?>`
  }
}

/* ── protobuf：无 schema 时呈现本身就是决策，所以决策住在这里 ── */

/**
 * wire type 的名字，出自 protobuf 官方 Encoding 文档。`wire=2` 那样的数字看不出
 * 是什么，名字看得出；不认识的（`Node` 可以由编辑路径重建）退回数字。
 *
 * 4（EGROUP）不在表里是有意的：它只能当 group 的终止符，`protobuf.ts` 从不把
 * 它解成一个字段。真出现在树里就该显示成 `wire=4`，那是个该被看见的异常。
 */
const WIRE_NAMES: Record<number, string> = {
  0: 'varint',
  1: 'fixed64',
  2: 'len',
  3: 'group',
  5: 'fixed32',
}

const WIRE_VARINT = 0
const WIRE_FIXED64 = 1
const WIRE_LEN = 2
const WIRE_FIXED32 = 5

/** 位重解释用的暂存区。set 与 get 用同一个字节序，所以取哪个都不影响结果。 */
const BITS = new DataView(new ArrayBuffer(8))

const U32_MAX = 0xffffffffn
const UINT64_MAX = 18446744073709551615n
/** 2^63：raw 到了这里，补码读法就是负数。 */
const INT64_SIGN = 9223372036854775808n

function renderField(node: Extract<Node, { t: 'field' }>, depth: number): string {
  const wire = WIRE_NAMES[node.wire] ?? `wire=${node.wire}`
  const guess = node.wire === WIRE_LEN ? guessTag(node.v) : ''
  return `#${node.no} ${wire} => ${guess}${render(node.v, depth)}${altReading(node.wire, node.v)}`
}

/**
 * wire type 2 装的是 string / bytes / 嵌套 message / packed repeated 中的哪一种，
 * 无 schema 时**只能猜**，所以要把猜测显式标出来 —— 用户得看见这是决策
 * 而不是事实（doc/frontend/codec-formats.md「Protobuf」）。
 *
 * 标注**推导**自子节点的类型，不存进 `Node`：`arr` 只可能来自试解成功的嵌套
 * message，`str` 只可能来自试 UTF-8 成功，`bytes` 是兜底。存一份反而会跟数据
 * 不同步。猜错也只影响这一行文字：往返走的是同一段字节，见 `protobuf.ts`。
 */
function guessTag(node: Node): string {
  switch (node.t) {
    case 'arr':
      return '~msg '
    case 'str':
      return '~str '
    case 'bytes':
      return '~bytes '
    default:
      return ''
  }
}

/**
 * 同一串位的另一种读法。
 *
 * wire 1 是 `fixed64` / `sfixed64` / `double` 共用的，wire 5 是 `fixed32` /
 * `sfixed32` / `float` 共用的，wire 0 上 `int64 -1` 与 `uint64 2^64-1` 的字节
 * 一模一样 —— 无 schema 时分不出来。所以 `protobuf.ts` 只收原始位（那是唯一
 * 无损的形态），"它可能是什么"在这里给出，这才是展示决策该待的地方
 * （doc/backend/codec.md「归属切分」）。
 *
 * 只显示歧义值得显示的那些：
 *
 *   - 定宽的那两种一律给出浮点读法 —— 光看 `4607182418800017408` 认不出这是 1.0
 *   - varint 只在 raw ≥ 2^63 时给出补码读法：那时它十有八九是个负的 `int64`，
 *     而那串二十位数字对读者毫无意义
 *   - **zigzag（`sint32` / `sint64`）不显示**：它对每一个值都有一种读法，
 *     `1` 会变成 `-1`，满屏都是就成了噪声而不是信息
 */
function altReading(wire: number, node: Node): string {
  if (node.t !== 'int' || node.v < 0n) return ''

  if (wire === WIRE_FIXED64 && node.v <= UINT64_MAX) {
    BITS.setBigUint64(0, node.v)
    return ` (double ${renderFloat(BITS.getFloat64(0))})`
  }
  if (wire === WIRE_FIXED32 && node.v <= U32_MAX) {
    BITS.setUint32(0, Number(node.v))
    return ` (float ${renderFloat(BITS.getFloat32(0))})`
  }
  if (wire === WIRE_VARINT && node.v >= INT64_SIGN && node.v <= UINT64_MAX) {
    return ` (int64 ${BigInt.asIntN(64, node.v)})`
  }
  return ''
}

function block(
  open: string,
  close: string,
  count: number,
  renderItem: (index: number) => string,
  depth: number,
): string {
  if (count === 0) return `${open}${close}`
  if (depth >= MAX_DEPTH) return `${open} … 嵌套过深，${count} 项未展开 ${close}`

  const pad = INDENT.repeat(depth + 1)
  const shown = Math.min(count, MAX_ITEMS)
  const lines: string[] = []
  // 先切再渲染：一个百万元素的数组不能为了丢弃而先全部渲染一遍
  for (let i = 0; i < shown; i += 1) lines.push(pad + renderItem(i))
  if (count > shown) lines.push(`${pad}… 还有 ${count - shown} 项未显示`)

  return `${open}\n${lines.join('\n')}\n${INDENT.repeat(depth)}${close}`
}

function renderBytes(bytes: Uint8Array): string {
  if (bytes.length === 0) return 'bytes(0)'
  const shown = Math.min(bytes.length, MAX_BYTES)
  const hex: string[] = []
  for (let i = 0; i < shown; i += 1) hex.push(bytes[i].toString(16).padStart(2, '0'))
  return `bytes(${bytes.length}) ${hex.join(' ')}${bytes.length > shown ? ' …' : ''}`
}

/** `-0` 与 `0` 在 msgpack / PHP 里是不同的字节，`String()` 会把它们抹平。 */
function renderFloat(value: number): string {
  return Object.is(value, -0) ? '-0' : String(value)
}
