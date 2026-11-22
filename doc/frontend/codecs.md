# 渲染进程编解码器

覆盖：`src/lib/codec/index.ts`、`src/lib/codec/types.ts`、`src/lib/codec/chain.ts`、`src/lib/codec/render.ts`、`src/lib/codecs.ts`、`src/components/keys/viewers/CodecPicker.vue`、`src/components/keys/viewers/ValueDrawer.vue`、`src/components/keys/viewers/StringViewer.vue`（解码链与保存）

字节 → 结构的 codec 在渲染进程里实现，归属理由见 [backend/codec](../backend/codec.md)「归属切分」；
各格式自己的事实见 [codec-formats](codec-formats.md)。

解码的统一入口是 `decodeValue`，按 `conclusion` 分派：压缩类走 IPC，序列化类查注册表。
加一种序列化 codec 就是一个新文件加注册表上一行（`RENDERER_CODECS` 或 `READONLY_CODECS`）：
注册集中在表上而不是各模块 import 时自注册，因为这张表是 `isImplemented` 的唯一依据，
自注册会引入「没人 import 就没注册」的顺序问题。

## 产物是结构不是文本

一次解码的产物是 `Decoded`：压缩类是 `{ kind: 'text', value: RedisText }`，序列化类是
`{ kind: 'struct', root: Node, editable: Editable }`。`Node` 的定义以 `src/lib/codec/types.ts` 为准。

**绝不能把 `Node` `JSON.stringify` 回 `RedisText`。** 把这些 codec 归给前端的理由正是「中间任何一次
JSON 化都有损」；躲开了 Rust 转 JSON 的损失、转头在前端自己转一次，这条归属理由就被自己推翻了。
谁要「简化」成那样，先看这一节。

两处形状是刻意的：

- `map` 是 `Array<[Node, Node]>` 而不是 `Record`——msgpack 允许非字符串 key，PHP 数组的 key 是
  int 与 string 混合的**有序**表，`Record` 会同时丢掉类型和顺序。
- `int` 是 `bigint` 而不是 `number`——int64 塞进 double 会静默丢精度。

`Node` 只活在渲染进程里，**不过 IPC**，所以 `types/ipc.ts` 与 IPC 契约不受它影响。

屏幕上的结构由 `render.ts` 渲染成带类型标注的展示文本（`i64 123` / `~msg` / `bytes(4)`）。
**它不是一个能解析回 `Node` 的格式，也不该是**——那等于把结构压成文本再读回来。所以结构没有
编辑路径，要编辑得有一个结构编辑器，见 [backlog](../backlog.md)「序列化结构解出来只能看」。

### Node 按格式扩展

`Node` 可以长，但只能按格式加变体，不能压回通用 JSON 形状——压回去丢的正是上面要保住的东西。
pickle 的 `seq`（tuple / set / frozenset）不能并进 `arr`，`call`（`GLOBAL` / `REDUCE`）不能并进 `seq`；
Java 有自己的 `java` 入口。各自的理由见 [codec-formats](codec-formats.md)「pickle」「Java 序列化」。

代价是所有对 `Node` 的穷尽 `switch` 都要同步：`render.ts` 加呈现，可往返解析器的 `encode` 对不属于
自己格式的变体明确抛错。`vue-tsc` 会精确指出位置——不改就编译不过，不会漏。

## 往返自检

序列化类解码后立刻重编，与原字节**逐字节**比对（`checkRoundTrip`）：相同才允许写回，不同就转只读，
并给出具体理由（第几个字节、长度差多少）。

理由是**同一个值不止一种字节表示**。「解码 → 编辑 → 编码」即使一个字节都没改，产物也可能与原文
不同——不是解错了，是原文用了另一种同样合法的写法：

- **PHP**：`serialize_precision` 决定 double 写几位，而且是运行时可配的 ini，同一个 `0.1` 在同一个
  Redis 里可能有两种字节（实测见 [codec-formats](codec-formats.md)「float.literal」）。
- **msgpack**：`1` 可以是 fixint `01`，也可以是 uint8 `cc 01`。主流库都写最短形式，所以非最短罕见但合法。
- **protobuf**：varint 允许冗余编码（`80 00` 也是 0），tag 与长度前缀同理。
- **PHP 的 `R:` / `r:`**：引用编号依赖序列化时的遍历顺序，编辑后重建会指错。
- **PHP 的 `C:`**：内容是类自己写的原始字节，格式不自描述，重编无从谈起。

所以**能不能安全写回只能靠机械验证，不能靠「哪些类型可往返」的清单**。那张清单必然漏——上面每一条
都是漏网的候选；机械验证一次全覆盖，失败时还能说出具体理由。这也是 `RendererCodec.encode` 必需的原因。

文本格式另有一层：解码时把原始字面量一起带上，没编辑过的值原样写回，不去猜它当初用的是哪种写法
（PHP 的 `float.literal`）。二进制格式表示唯一，不需要这一层。

不可往返与截断是同构的（都是「这个值不能安全写回」），复用同一套只读机制，见
[backend/ipc-contract](../backend/ipc-contract.md)「截断契约」。

## 截断守卫

**半截 msgpack 跟半截 gzip 一样解不开。** `totalBytes` 非空时，压缩类与序列化类一律拒绝解码，说明
「值已截断，格式需要完整字节」，而不是解出一个残缺结构。压缩类如果不拦，会以 `codec.decode_failed`
的形式失败，**看起来像「这不是 gzip」**——用户会以为自己认错了格式，而不是意识到值太大。

守卫在 `chain.ts` 的 `extendBlockedReason`：链的**每一步**都要守，中途解出来的一层同样可能带截断。
`extendChain` 被拦下时抛错，`CodecPicker` 的菜单在点之前就置灰，两处共用 `truncatedReason` 的措辞。

拒绝之后要给得出下一步，否则用户只是撞墙：`ValueDrawer` 的截断横幅在有定位符时给「加载完整值」，
没有定位符（Set / ZSet 成员）时用 `noTargetReason` 说清**为什么**给不出。重取本身见
[backend/ipc-contract](../backend/ipc-contract.md)「完整值重取」。

## 识别的三个维度

| 维度 | 问的是 | 住在哪 |
|---|---|---|
| `isImplemented` | 能不能跑 | `lib/codec/index.ts`，按 conclusion 查对应的注册表 |
| `detectable` | 该不该进自动识别的候选集 | `lib/codecs.ts` 的 `CodecInfo` |
| `detectPrefix` | 进了候选集之后，试解之前还要满足什么 | codec 自己（`ReadonlyCodec`） |

三个维度各管各的，**不要让后一个跟着前一个走**。候选集 `DETECT_CANDIDATES` 跟着 `detectable`，
不跟着「能不能跑」——否则每实现一个解析器候选集就自动变长，而 msgpack / protobuf 恰恰是实现了也不能进
的那两个：无魔数，任意字节都可能是合法 msgpack 或合法 protobuf，进了候选就会把一切都识别成它。
每个 codec 进不进候选、理由是什么，以 `CodecInfo.detectable` 的注释为准。

`detectCodec` 分两趟：先在本地按候选顺序试渲染进程的解析器，没命中再把同一份候选集交给后端的
`native_codec_detect` 试压缩类（后端会跳过它跑不了的）。本地这一趟必需：后端没有序列化类的实现。

候选集里的判据是「decode 抛不抛错」，所以**解析器必须严格**：吃完全部输入、违反就抛，不返回
「部分解出来」的结构。宽松的解析器会把 `a:1:{i:0;i:1;}` 后面跟着的任意垃圾也判成成功，自动识别就废了。

但严格还不够，所以有第三个维度。`N.` 两个字节就是一段合法 pickle（`NONE` + `STOP`），任何以合法
opcode 开头、恰好以 `.` 结尾的短值都会被它认领。消费完整性挡的是「解到一半就散了」，挡不住「整段
恰好合法」。pickle 的 `detectPrefix` 因此要求 protocol 2+ 的 `PROTO` 前缀。

**手点走的是第一个维度**，所以 protocol 0 / 1 的 pickle 照样解得开，只是不会被自动认出来。

自动识别由 `CodecPicker` 的「自动识别」菜单项触发，识别出的 codec 作为链上的一步应用。
msgpack / protobuf 因此只能手点，见 [backlog](../backlog.md)「msgpack 与 protobuf 只能一个一个手点」。

## 链

`gzip → msgpack` 这类组合很常见，所以解码是一条链：`chain.ts` 的 `CodecChain`（`source` + `steps`，
每一步连**产物**一起存）。每一步的输入是上一步的输出；链是不可变的，每次操作返回新对象——
装它的 ref 是 `shallowRef`，只认整块替换。

链是一个类型而不是一个 codec 名数组，因为它要撑起三件事：

- **回退**要拿得出上一层的字节。存了产物，退一步是 `undoStep`，不重解、不发 IPC。
- **判「链到头了」**要知道链尾是不是结构（`isTerminal`）。`Node` 不是字节，套不了下一层，所以 struct
  那一步**永远是最后一步**。这条不变量让「反向 encode 只会遇到压缩类」成立。
- **反向 encode** 要知道有哪几层。`encodeAlongChain` 返回**每一层**的字节而不只是最外层：保存之后
  `rebaseChain` 拿它把链上的字节换成刚写进去的那份，否则「还原原始值」会掏出一份已经不在 Redis 里的
  旧值——那是界面在说「这就是库里的东西」，最难被怀疑的一种谎。层序也因此可以被断言。

`CodecPicker` 是**受控组件**：链归调用方，因为保存要沿链反向 encode，那份状态必须待在有保存路径的
一侧。抽屉换行时调用方重建链，按钮上不会留着上一行的 codec 名。

`StringViewer` 的保存走 `encodeAlongChain`。**压缩类的链完全可编辑**（gzip 解开 → 改 JSON → 存回去
仍然是 gzip），那是最常见的真实用例；不按链反向 encode 的话，解码后点保存会把明文写回去、覆盖掉原本
的压缩值。编辑过之后再接一步，解的是屏幕上那份（`codecSource`）：链只记录用过哪些 codec，反向 encode
是对这张表求逆，中途改过字节不影响它成立。

`StringViewer` 的只读来源合在 `readOnlyNow`：会话只读、截断、**这一层不是 UTF-8**、链尾是结构。
二进制那一条与截断、结构是同一件事——编辑器里那串十六进制是渲染产物，照着存等于把十六进制文本当成值
写进 Redis。它不影响压缩链的可编辑性：解出来的 JSON 是 UTF-8。

`ValueDrawer` 没有保存路径，链在那里只影响展示；重取完整值之后整条链重开。

readonly 类的产物同样是 `struct`，所以链一行不用为它们改：struct 永远是链尾，反向 encode 只会遇到压缩类
（`encodeStep` 只接受 `rustRoundTrip`）。`gzip → pickle` 天然走得通，而它是真实用例——Django 的
cache backend 默认就是 zlib 压缩 + pickle。

## ReadonlyCodec

`RendererCodec` 的 `encode` 是必需的，往返自检靠它。pickle 与 Java 序列化按定义没有 encode
（[backend/codec](../backend/codec.md)「Readonly 安全边界」），所以它们走**第二个接口 + 第二张注册表**：
`ReadonlyCodec`（`decode` 加可选的 `detectPrefix`）与 `READONLY_CODECS`。

- `isImplemented` 与 `localDecoderFor` **按 conclusion 分派到对应的那一张表**，不是两张都翻一遍：
  把 readonly 的实现挂进 `RENDERER_CODECS` 不算数，那张表的契约含 encode。`codec.test.ts` 钉着这一条。
- `conclusion` 本身不能动：改成 `rendererRoundTrip` 会让 `CONCLUSION_LABEL` 显示「可解可编」，
  而它们只能解。
- 产物形状与 `RendererCodec` 一致，`editable` 是常量 `{ ok: false, kind: 'noEncoder' }`（`readonlyEditable`）。
  界面那一侧不必知道值从哪张表来，只看 `editable`。
- 没有自检，就没有了解析器自己的运行时断言。替代物是**消费完整性**，见
  [codec-formats](codec-formats.md)「消费完整性」。

## Editable 的三种说法

`Editable` 决定界面说哪一句话。三种**性质各不相同**，文案不能混：

| `Editable` | 性质 | 说法 |
|---|---|---|
| `ok: false, kind: 'lossy'` | **数据**：重编回去就不是原字节了 | 编不回原来的字节（附具体理由），照这份呈现保存会改写这个值 |
| `ok: false, kind: 'noEncoder'` | **格式**：我们不给这个格式写编码器 | 这是安全决策；值本身没有问题，能看，但不提供写回 |
| `ok: true` | **工具**：结构完整可逆，缺的是结构编辑器 | 数据本身没有问题，缺的是编辑器 |

用户看到「只读」会以为是自己的数据有问题，而答案常常相反，所以要照实说。`noEncoder` 尤其不能落进
`STRUCT_LOSSY_PREFIX`（「解出来的结构编不回原来的字节」）：对 pickle 那是假话，它编得回去，是我们不编。

- `StringViewer` 的保存理由由 `chain.ts` 的 `saveBlockedReason` 给，三种各一句。
- `ValueDrawer` 的横幅**只给 `lossy`**：它说的是「下面这份呈现是有损的」。`noEncoder` 的呈现完整准确，
  而「不能保存」在一个本来就没有保存按钮的抽屉里不值一提；`ok: true` 时呈现无损，也什么都不说。

靠判别字段 `kind` 分，**不要靠匹配 `reason` 字符串**——那是把文案当协议用，改一个字就断。

## 不做

- **`RendererCodec.encode?:` 可选。** 可选的 `encode` 意味着将来任何一个解析器都能靠「不写 encode」
  绕过往返自检，而自检正是「不列类型清单」这条设计的全部依托。只读格式走 `ReadonlyCodec`。
- **用类型清单判可编辑性。** 见「往返自检」。
- **把结构渲染成可读回的文本（如 JSON）当编辑路径。** 那就是把结构压成文本再读回来，见
  「产物是结构不是文本」。要编辑得做结构编辑器。
- **候选集跟着 `isImplemented` 走。** 见「识别的三个维度」。
