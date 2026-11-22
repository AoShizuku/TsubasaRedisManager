import type { RedisText } from '@/types/ipc'

/**
 * Java Object Serialization 的 class descriptor。
 *
 * 它只记字节流里自描述的事实，不去找 classpath、也不加载类。proxy descriptor
 * 在 wire format 里没有类名 / SUID / flags / 字段表，所以这些格子照实是 `null`
 * 或空数组，接口名则单独保留。
 */
export interface JavaClassLayer {
  kind: 'class' | 'proxy'
  name: string
  serialVersionUid: bigint | null
  flags: number | null
  interfaces: string[]
  fields: JavaFieldDescriptor[]
  annotations: JavaContent[]
}

/** `type` 是 wire format 里的 `B/C/D/F/I/J/S/Z/L/[`，`className` 是 L/[ 的 descriptor。 */
export interface JavaFieldDescriptor {
  type: string
  name: string
  className: string | null
}

/** 一个 class layer 的字段值；父类与子类分层存，所以同名字段不会互相覆盖。 */
export interface JavaFieldValue {
  descriptor: JavaFieldDescriptor
  value: Node
}

/**
 * `SC_WRITE_METHOD` / protocol 2 Externalizable / class annotation 的内容。
 * primitive write 会被 ObjectOutputStream 包成 blockdata；对象仍带 TC 标签，可以继续解析。
 */
export type JavaContent =
  | { kind: 'block'; value: Uint8Array }
  | { kind: 'value'; value: Node }
  | { kind: 'reset'; epoch: number }

export interface JavaClassDataLayout {
  fields: JavaFieldValue[]
  custom: JavaContent[]
  /** 默认字段出现在第几个 custom content 之后；null 表示没调用或该 layer 不适用。 */
  defaultFieldBoundary: number | null
}

export interface JavaClassData extends JavaClassDataLayout {
  descriptor: JavaClassLayer
  /** wire 上存在多个等价边界时全部列出，展示层必须显式标成二义，不能偷偷选一个。 */
  alternatives?: JavaClassDataLayout[]
}

/**
 * Java 专用的结构节点。
 *
 * 不把这些信息硬塞进 `obj` / `arr`：Java 必须同时保留 declaring class、字段
 * descriptor、父类顺序、blockdata 与 handle reset epoch。尤其 `reference` 不能
 * 借 pickle 的 `call` 表示——Java 引用不是一次调用，混用会制造错误的安全信号。
 */
export type JavaValue =
  | { kind: 'object'; className: string; classes: JavaClassData[] }
  | { kind: 'array'; className: string; values: Node[] }
  | {
      kind: 'primitiveArray'
      className: string
      component: string
      count: number
      bytes: Uint8Array
    }
  | { kind: 'enum'; className: string; name: Node; classes: JavaClassLayer[] }
  | { kind: 'class'; classes: JavaClassLayer[] }
  | { kind: 'classDesc'; classes: JavaClassLayer[] }
  | { kind: 'reference'; epoch: number; handle: number; target: string }
  | { kind: 'stream'; contents: JavaContent[] }
  | { kind: 'exception'; value: Node }

/**
 * 序列化类编解码器的产物：一棵**保留类型**的结构树。
 *
 * 这个定义就是规格，文档只引用它（doc/frontend/codecs.md「产物是结构不是文本」）。
 * 可以按格式加变体，但不要把已有变体压回通用 JSON 形状，见同文档「Node 按格式扩展」。
 *
 * **绝不能把 `Node` `JSON.stringify` 回 `RedisText`。** doc/backend/codec.md「归属切分」把
 * msgpack / phpSerialize / protobuf 归给前端，理由正是「中间任何一次 JSON 化都有损」：
 * int64 精度、binary 字段、非字符串 map key 会全丢。躲开了 Rust 转 JSON 的损失、
 * 转头在前端自己转一次，等于把这条归属理由自己推翻。
 *
 * 好性质是它**不过 IPC**：`types/ipc.ts` 与 IPC 契约（doc/backend/ipc-contract.md）一个字都不用改。
 *
 * 两处形状是刻意的，别"简化"：
 *   - `map` 是 `Array<[Node, Node]>` 而不是 `Record` —— msgpack 允许非字符串 key，
 *     PHP 数组的 key 是 int 与 string 混合的**有序**表，`Record` 会同时丢掉类型和顺序
 *   - `int` 是 `bigint` 而不是 `number` —— int64 塞进 double 会静默丢精度
 *
 * `float.literal` 见下。
 */
export type Node =
  | { t: 'null' }
  | { t: 'bool'; v: boolean }
  | { t: 'int'; v: bigint } // msgpack int64 / php i:
  | { t: 'float'; v: number; bits: 32 | 64; literal?: string }
  | { t: 'str'; v: string }
  | { t: 'bytes'; v: Uint8Array }
  | { t: 'arr'; v: Node[] }
  | { t: 'map'; v: Array<[Node, Node]> } // key 可以不是字符串
  | { t: 'obj'; cls: string; v: Array<[Node, Node]> } // php O: / E:
  | { t: 'field'; no: number; wire: number; v: Node } // protobuf
  | { t: 'ext'; code: number; v: Uint8Array } // msgpack ext
  | { t: 'seq'; cls: string; v: Node[] } // pickle tuple / set / frozenset
  | { t: 'call'; fn: string; v: Node[] | null; state?: Node; data?: Node } // pickle GLOBAL / REDUCE
  | { t: 'java'; v: JavaValue }

/**
 * `seq` / `call` 两个变体是给 pickle 的，论证在 doc/frontend/codec-formats.md「pickle」。
 *
 * **`seq` 不能并进 `arr`**：Python 的 `list` 与 `tuple` 是不同类型，`set` 与
 * `frozenset` 也是。挤进 `arr` 之后屏幕上 `[1, 2, 3]` 分不出是哪一种 —— 这跟
 * 上面列的"int64 精度、binary 字段、非字符串 map key"是同一类损失，只是换了格式。
 *
 * **`call` 不能并进 `seq`**：`tuple` 无害，`REDUCE` 是那个危险的东西（反序列化
 * 即执行，`call` 记的就是"用 Python 加载时会调用什么"）。两者塞进同一个变体，
 * 最重要的信号会被淹在最常见的噪声里 —— 每个 REDUCE 的参数**本身就是个 tuple**，
 * 它俩在同一棵树上密集共存。这与 protobuf"把猜测显式标注"同一条原则，
 * 只是这里标注的是**危险**而不是猜测。
 *
 *   `v: null`      裸的 `GLOBAL`，只是一个引用，没有被调用
 *   `v: Node[]`    `REDUCE` / `INST` / `OBJ` / `NEWOBJ`，参数就是这几个
 *   `state`        `BUILD` 补上的对象状态（`__setstate__` / `__dict__`）
 *   `data`         `APPENDS` / `SETITEMS` / `ADDITEMS` 往这个对象里塞的内容
 *
 * `Foo` 与 `Foo()` 在 pickle 里是两串不同的字节，而**只有后者会执行** ——
 * 所以那个 `null` 不是可有可无的省事写法。
 *
 * 后两个是可选字段而不是新变体：它们永远作用在一个**已经存在的**对象上
 * （Python 那边 `load_build` / `load_appends` 取的都是栈顶），合并进去比包一层
 * 更贴近事实。`state` 与 `data` **分开**是因为它们是两回事 —— 一个
 * `collections.OrderedDict` 可以既有 items（`data`）又有 `__dict__`（`state`），
 * 合并会把这个区别抹掉。
 */

/**
 * 文本格式里 float 的**原始字面量**，只有从文本格式解出来时才有。
 *
 * 为什么需要它：同一个 double 有多种合法写法，而往返自检要的是**字节相同**。
 * PHP 的 `serialize_precision` 决定写几位有效数字，**它是运行时可配的 ini**：
 * 5.6 默认 17 写 `d:0.10000000000000001;`，7.1+ 默认 -1（最短往返）写 `d:0.1;`
 * —— 同一个 0.1，两串不同的字节，而且不只是版本迁移，一台机器上改个 ini 就能
 * 让两种数据并存在同一个 Redis 里。从 `number` 重建只能顾一头，另一头就被误判
 * 成不可编辑。（实测三个版本见 `php.ts` 的 `phpDoubleText`。）
 *
 * 这**不是**"把原始字节藏起来让自检必然通过"那种捷径，判据有两条：
 *
 *   1. 只保留**表示的选择**（同一个值的多种写法），不保留值本身以外的任何东西；
 *      写回前还要校验 `literal` 解析出来确实等于 `v`，不一致就丢掉它重新格式化，
 *      所以它写不出错误的值
 *   2. 用户改了这个 float，新节点就没有 `literal`，自然回到重建路径
 *
 * 自检因此仍然有意义：它保护的是"编辑后能不能安全写回"，而 `literal` 只是让
 * "根本没编辑过的值"不要被误判。二进制格式表示唯一，不需要也不该填这个字段 ——
 * msgpack 的 float32 / float64 就是这么处理的；protobuf 压根不产出 `float`
 * 节点（wire 1 / 5 未必是浮点，收的是原始位，见 `protobuf.ts` 与
 * doc/frontend/codec-formats.md「wire 1 / 5 收成整数」）。
 */

/**
 * 能不能把编辑结果写回去。
 *
 * 判据是**往返自检**（解码后立刻 re-encode 逐字节比对），不是一张"哪些类型可往返"
 * 的清单 —— 那张清单必然漏，见 `index.ts` 的 `checkRoundTrip`。
 * 与 `isTruncated` 是同构的（都是"这个值不能安全写回"），所以复用同一套只读机制。
 *
 * **`kind` 不是装饰，它决定界面说哪一句话。** 用户看到"只读"会以为是自己的数据
 * 有问题，而答案常常相反，所以要分开说，一共三种
 * （doc/frontend/codecs.md「Editable 的三种说法」）：
 *
 *   `lossy`      **数据**的性质 —— 这个值重编回去就不是原字节了
 *                （PHP 的 `R:` / `C:`、msgpack 的非最短编码、protobuf 的冗余 varint）
 *   `noEncoder`  **格式**的性质 —— 我们不给这个格式写编码器，见
 *                doc/backend/codec.md「Readonly 安全边界」。
 *                注意它跟 `lossy` 是**相反**的处境：pickle 编得回去，是我们不编
 *   `ok: true`   **工具**的能力 —— 结构完整可逆，缺的只是结构编辑器
 *
 * 判别字段而不是匹配 `reason` 字符串：后者是把文案当协议用，改一个字就断。
 */
export type Editable =
  | { ok: true }
  | { ok: false; kind: 'lossy' | 'noEncoder'; reason: string }

/**
 * 一次解码的产物。两类编解码器在这里汇合：
 *
 *   - 压缩类（`rustRoundTrip`）是**字节 → 字节**，产物仍然是 `RedisText`，
 *     后端解完回来的那份
 *   - 序列化类（`rendererRoundTrip`）是**字节 → 结构**，产物是上面的 `Node`
 *
 * 切分线见 doc/backend/codec.md「归属切分」。
 */
export type Decoded =
  | { kind: 'text'; value: RedisText }
  | { kind: 'struct'; root: Node; editable: Editable }
