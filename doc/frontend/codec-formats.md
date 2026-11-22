# 序列化格式

覆盖：`src/lib/codec/php.ts`、`src/lib/codec/msgpack.ts`、`src/lib/codec/protobuf.ts`、`src/lib/codec/pickle.ts`、`src/lib/codec/java.ts`

五个解析器各自的格式事实：为什么这样解、哪里解不全、哪里必须拒绝。框架（往返自检、识别、链）见
[codecs](codecs.md)，安全边界见 [backend/codec](../backend/codec.md)「Readonly 安全边界」。

## 依赖：全部自写

五个解析器都是自写的，没有运行时依赖。判据：**依赖买的是「格式细节的正确性」和「我们不想自己维护的
编码器」，不是省几行代码。** 现有的 JS 库在这几个格式上要么卖不出前者，要么形状不对：

| codec | 不用库的理由 |
|---|---|
| phpSerialize | JS 库普遍搞错字节语义，见「PHP serialize」 |
| msgpack | 库的值模型是惯用 JS 值，装不下 msgpack，见「不用 @msgpack/msgpack 的五条理由」 |
| protobuf | `protobufjs` 是 schema 驱动的，我们没有 `.proto`；需要的是 wire-format 探查器 |
| pickle / Java | 只能做不实例化的结构解析，语言运行时的反序列化 API 正是要避开的东西 |

自写的要在文件头写明规范出处（msgpack spec / PHP 手册与 `var_unserializer.re` 的文法 / protobuf
Encoding 文档 / `pickletools` 与 PEP / Java Object Serialization Specification）。CLAUDE.md 那条「独立编写」
不能靠声明，要靠可追溯的来源。

共同要求：**decode 抛错 = 不是这个格式**，不返回「部分解出来」的结构；嵌套深度有上限（以各文件的
`MAX_DEPTH` 为准），递归下降炸栈抛的 `RangeError` 混在「解不开」里看不出发生了什么。

## PHP serialize

**输入全程按字节走。** `s:5:"héllo"` 里的 5 是**字节数**；JS 库先把输入 `TextDecoder` 成 string 再 parse，
长度就成了 UTF-16 code unit 数，非 ASCII 立刻错位。`s:` 的内容只有确认是合法 UTF-8 才解成 `str`，
否则进 `bytes`——PHP 的 string 是字节串，塞一串 U+FFFD 进去等于篡改数据。

要处理的 token：`N;` `b:` `i:` `d:`（含 `NAN` / `INF` / `-INF`）`s:` `a:` `O:` `R:` `r:`，PHP 8.1 的 `E:`（enum），
`S:`（带 `\xx` 转义的字符串），`C:`（`Serializable` 的自定义序列化）。

- **`C:` 的内容是类自己写的原始字节，格式不自描述**——只能当 `bytes` 展示，必然不可往返。不特判，
  往返自检会把它判成只读。
- **`R:` / `r:` 的引用编号**依赖 PHP 序列化时的遍历与计数规则，编辑后重建会指错，同样交给自检。
- **`O:` 解出类名与属性，不实例化任何东西**，所以 phpSerialize 是 `RendererRoundTrip`，理由见
  [backend/codec](../backend/codec.md)「Readonly 安全边界」末段。

### float.literal

**同一个 double 有多种合法写法，而往返自检要的是字节相同。** PHP 的 `serialize_precision` 决定写几位
有效数字，实测（`docker run --rm php:{5.6,7.4,8.3}-cli`）：

| | 5.6（`serialize_precision=17`） | 7.4 / 8.3（默认 `-1`） |
|---|---|---|
| `0.1` | `d:0.10000000000000001;` | `d:0.1;` |
| `1e-5` | `d:1.0000000000000001E-5;` | `d:1.0E-5;` |
| `100.0` | `d:100;` | `d:100;` |
| `1e17` | `d:1.0E+17;` | `d:1.0E+17;` |

要点是 **`serialize_precision` 是运行时可配的 ini，不只是版本差异**——一台机器上改个设置，两种写法就
并存在同一个 Redis 里。没有哪一种是「对」的，「按当前 PHP 的习惯重新格式化」这条路走不通。

PHP 7.1+ **不**给整数值的 double 补 `.0`，三个版本写的都是 `d:100;`。`smart_str_append_double` 的
`zero_frac` 是 `var_export` / `json_encode` 那条路径的行为，`serialize()` 不走它。别照着「补 `.0`」去修正实现。

所以解码时把原始字面量记进 `Node.float.literal`，写回时原样吐出去（`php.ts` 的 `doubleText`）。
这**不是**「把原始字节藏起来让自检必然通过」的捷径：

1. 只保留**表示的选择**，不保留值以外的任何东西；写回前校验 `literal` 解析出来确实等于 `v`
   （`sameDouble`），不一致就丢掉重新格式化，所以它写不出错误的值。
2. 用户改了这个 float，新节点就没有 `literal`，自然回到重建路径（`phpDoubleText`）。

自检因此仍然有意义：它保护的是「编辑后能不能安全写回」，`literal` 只是让**根本没编辑过**的值不被误判。

`literal` 会被**原样拼进输出**，而 `Node` 可以由编辑路径重建，不能假设它只来自自家解析器。所以校验里
必须带文法检查，否则一个构造出来的 `1;s:4:"evil";` 会把额外的序列化数据注进去。

二进制格式表示唯一，不填这个字段：msgpack 的 float32 / float64 就是这么处理的，protobuf 压根不产出
`float` 节点。

## MessagePack

- str 与 bin 是两族：str 规定是 UTF-8，bin 是任意字节。str 族的内容不是合法 UTF-8 时进 `bytes`，代价是
  重编会写成 bin、字节对不上、自检判只读——**宁可只读，也不能把 U+FFFD 写回 Redis**。
- float32 / float64 的宽度记在 `Node.float.bits`，原样写回。
- ext（包括 type -1 的 timestamp）原样进 `Node.ext`，不解释成日期。
- 重编一律写**最短形式**。非最短编码的输入重编后对不上，自检判只读；这是正确行为，不开特例。
- 无魔数，**永远不进自动识别的候选集**。

### 不用 @msgpack/msgpack 的五条理由

以 3.1.3 核对。根子是同一个：库的公开 API 收发的是**惯用 JS 值**，那正是「JSON 化有损」时列的那些损失。

1. **map 只能解成普通 JS 对象。** v3 没有 `useMap` 这类开关，`mapKeyConverter` 的返回类型写死
   `string | number`。`{"b":1, 1:"x", "a":2}` 解出来键序变成 `1, b, a`（整数样式的键被 JS 的属性顺序
   规则提到最前），int 键 `1` 变成字符串 `"1"`——类型与顺序一次全丢。
2. **`useBigInt64` 在写的一侧反而破坏保真。** bigint 一律写满 8 字节，`[1,2,3]`（`93 01 02 03`）重编出来
   21 字节。而 `Node.int` 只有 bigint 一种形态，于是**每一个含小整数的值都会被自检判成只读**。
3. **float32 / float64 是全局开关**（`forceFloat32`），没有逐值的写法；解出来又只是个 `number`，宽度当场
   就没了，`Node.float.bits` 无从填起。
4. **非 UTF-8 的 str 拿不回原字节。** `rawStrings` 的文档写明「不适用于 map 的键」，str 键 `ff fe` 被解成
   `U+00FF U+00FE`，重编是另外四个字节。
5. **`__proto__` 当键会被 `DecodeError` 拒掉**，而那是合法 msgpack。

**谁想把这条依赖加回运行时，先把这五条逐条复核一遍。**

它留在 devDependency 里，在 `msgpack.test.ts` 当**独立实现的对照**：我们写的字节它得读成同一个值，它写的
字节我们得逐字节往返。后者顺带钉住「主流写库只写最短形式」这个前提——「非最短判只读」不会误伤正常数据。
这才是「格式细节的正确性」真能从它那里买到的一层。

## Protobuf

无 schema 的 wire-format 探查器：只解得出字段号、wire type 与 payload。message 是一串记录，所以解成
`arr` of `field` 而不是 `map`——同一个字段号可以重复出现（repeated），顺序也是字节的一部分。
`.proto` schema 导入是独立特性（文件选择、schema 存储、类型解析一整套），目前没有。

### 呈现是猜测，必须标出来

wire type 2（LEN）可能是 string / bytes / 嵌套 message / packed repeated。按「哪个更可能有用」猜：
**递归试解为嵌套 message → 试 UTF-8 → 退回字节**，并把猜测显式标注（`~msg` / `~str` / `~bytes`）——
用户得看见这是决策而不是事实。

- **标注不存进 `Node`，由子节点类型推导**（`render.ts` 的 `guessTag`）：`arr` 只可能来自试解成功的嵌套
  message、`str` 只可能来自试 UTF-8 成功、`bytes` 是兜底。存一份反而会跟数据不同步——编辑路径改得到
  子节点，改不到那个标记。猜测归展示层，`Node` 那一层只放无损的事实。
- **「试 UTF-8」还要求没有 C0 控制字符**（`\t` `\n` `\r` 除外）。`01 02 03` 是合法 UTF-8，猜成字符串就是
  三个不可见字符，而 packed repeated 与短二进制里这种字节到处都是。这一条只改猜测，不改往返。

**猜测只影响展示，不影响往返**——这比标注更要紧，因为它可以机械验证：

- 猜成 message 时，长度前缀跟着**内部重编的实际长度**走，不是记下来的原长度。内部有非最短 varint 时
  整条记录跟着对不上，自检如实判只读，猜测没有把这个事实盖掉。
- 猜成 string 时靠的是 UTF-8 在合法序列上是双射（`TextDecoder` 的 `fatal` + `ignoreBOM`）。

所以「猜错」的代价只有一行文字，不会变成数据损坏。`protobuf.test.ts` 钉着这一条。

### wire 1 / 5 收成整数

wire 1 是 `fixed64` / `sfixed64` / `double` 共用的，wire 5 是 `fixed32` / `sfixed32` / `float` 共用的，无 schema
时分不出来。所以它们收成原始位的 `int`，**不是 `float`**：

- 收成 `float` 是把一个**猜测**塞进本该无损的那一层。
- 它还会让往返依赖一条**实现定义**的行为：小的负 `sfixed32` / `sfixed64`（-1、-2、-100…）的字节全落在
  IEEE 754 的 NaN 位型上，而 ECMAScript 对 `DataView.setFloat32/64` 写 NaN 只承诺「某个实现定义的位型」。
  V8 保位，但 webview 在 macOS / Linux 上是 JSC，JSC 的 NaN-boxing 会 purify 非规范 NaN：`sfixed32 = -1`
  这种再普通不过的值会被自检误判成只读，而 `Node` 里的 double 已经丢了原位型，复原都复原不回来。

收原始位没有这个问题，**而且信息更多**：`render.ts` 把浮点读法（`i64 1065353216 (float 1)`）与 varint
≥ 2^63 时的补码读法一起显示出来。zigzag 不显示：它对每个值都成立，`1` 会变成 `-1`，那是噪声。
varint 同理：`int64 -1`、`uint64 2^64-1` 与 `sint64` 的 zigzag 在字节上一样，一律收成原始无符号整数。

### group

wire 3 / 4（group）**忠实解析，不拒绝**：它仍在 Encoding 文档里（标着 deprecated），proto2 的老数据里真实
存在；没有长度前缀，所以往返比 LEN 记录还干净；而「结束标记的字段号必须对得上、且不能跨越 LEN 边界」
顺带成了嵌套 message 猜测的一道过滤。不平衡的三种情况（落单、号对不上、到末尾没闭合）一律抛错。

### 嵌套上限

上限比另几个解析器低，出处是 protobuf 官方实现自己的默认递归上限。第二个理由是这里独有的：LEN 记录要
试解成嵌套 message，同一段字节每层都要重扫一遍，开销是 O(输入长度 × 深度)，上限直接就是那个乘数。
超限时 LEN 记录**降级**成字节（照样忠实往返），group 跳不过去所以只能抛错。

## pickle

**pickle 完全自描述。** 它是 opcode 流，每个字节都有明确语义——这正是它能 RCE 的同一个原因（它是台
虚拟机）。所以纯解析能解出**完整**结构，包括 `GLOBAL` / `REDUCE` 的调用点；protocol 0–5 都解。
它没有 encode，见 [backend/codec](../backend/codec.md)「Readonly 安全边界」。

Python 的值模型有现有 `Node` 变体装不下的东西，所以加了两个变体（定义以 `types.ts` 为准）：

- **`seq` 不能并进 `arr`**：`list` 与 `tuple` 是不同类型，`set` 与 `frozenset` 也是。挤进 `arr` 之后屏幕上
  `[1, 2, 3]` 分不出是哪一种——跟 int64 精度、非字符串 map key 是同一类损失。
- **`call` 不能并进 `seq`**：`tuple` 无害，`REDUCE` 是那个危险的东西。每个 `REDUCE` 的参数本身就是个
  tuple，两者在同一棵树上密集共存，合并会把最重要的信号淹在最常见的噪声里。这与 protobuf「把猜测
  显式标注」是同一条原则，只是这里标注的是**危险**。
  - `v: null` 是裸的 `GLOBAL`：只是一个引用。`Foo` 与 `Foo()` 在 pickle 里是两串不同的字节，只有后者会执行。
  - `state` 是 `BUILD` 补上的对象状态，`data` 是 `APPENDS` / `SETITEMS` / `ADDITEMS` 塞进去的内容。它们是
    可选字段而不是新变体，因为永远作用在一个已经存在的对象上（栈顶）。两者必须分开：
    `collections.OrderedDict` 的 items 落在 `data`，同时可以有 `__dict__` 落在 `state`。
  - `BUILD` 的栈顶不是 `call`（例如 `EMPTY_DICT` + `BUILD`）时抛错——那种流 Python 自己 unpickle 也会失败。

`render.ts` 把 `call` 渲染成函数调用的语法本身，不另加警示前缀：绝大多数 `REDUCE` 完全正常
（`collections.OrderedDict`、`datetime.datetime`），逐个打危险标签就是狼来了。

protocol 5 的 out-of-band buffer（`NEXT_BUFFER` / `READONLY_BUFFER`）**直接拒绝**：数据根本不在字节流里，
Redis 里存不下这种值，遇到就是数据不对或者有人在试探。拒绝时说清是哪一种，不笼统报「解析失败」。

自动识别只认 protocol 2+（`detectPrefix`），见 [codecs](codecs.md)「识别的三个维度」。0 / 1 在今天极罕见
（Python 3 的默认 protocol 从 3 起步），放宽前缀会重蹈 msgpack 的覆辙；手点照样解得开。

## Java 序列化

**Java 部分不自描述。** `SC_WRITE_METHOD` 下 `writeObject` 写的 annotation 里，原始类型落在 `TC_BLOCKDATA`
块中、不自描述，但**嵌套对象仍然带 TC 标签**。所以一个 `HashMap` 解出来是：一段 blockdata（capacity /
size 那几个 int，只能当字节摆着）+ 后面一串正常解析的 key / value 对象。**内容看得见，header 看不懂**——
可用，但界面要如实标出那段 hex 是什么。这跟 PHP 的 `C:` 是同一类东西。

难度比 pickle 大，工作量在 classDesc 的字段类型表、父类链（从父到子读 classdata）、handle 引用表。

- **默认字段的位置没有 marker。** 带 `SC_WRITE_METHOD` 的 descriptor 仍会列出默认字段，但 `writeObject`
  可以完全不调 `defaultWriteObject()`，也可以在它前后写 optional content。解析器同时尝试「省略默认字段」
  与各个可能的插入点（插入点数有上限）。零个候选说明格式不完整；一个候选直接采用；多个候选只有在最终
  字节偏移与 handle 状态完全一致时才作为 `alternatives` 全部保留并显式标成二义，状态不一致则拒绝。
  `defaultFieldBoundary` 记住默认字段原来位于第几个 custom content 之后，呈现不能把它挪回最前面。
- **`TC_RESET` 只允许出现在顶层 content**，嵌在对象或 annotation 里直接拒绝。
- **`TC_EXCEPTION` 是特殊控制流**：前后各 reset 一次 handle，并中止当前整棵 `writeObject`。解析器 unwind 到
  顶层、丢掉已经读出的半截外层对象，不把 exception 当成一个普通字段值继续读。
- **Externalizable 只认 protocol 2。** `SC_EXTERNALIZABLE | SC_BLOCK_DATA` 的原始数据被 blockdata 包住、由
  `TC_ENDBLOCKDATA` 收尾，边界能机械找到。`writeExternal` 负责整个对象，不沿父类链逐层写字段，所以只读
  **最派生类的一段** annotation；父类 descriptor 保留用于展示，但不为它们臆造字段。protocol 1 没有
  `SC_BLOCK_DATA` 也没有结束标记，只有原始类的 `readExternal` 知道要读多少——带具体原因拒绝。

`Node` 有专门的 `java` 入口（`JavaValue`，定义以 `types.ts` 为准），不硬塞进 `obj` / `arr`：Java 必须同时保留
declaring class、字段的 wire descriptor、父子类层次、class annotation、custom data、handle 与 reset epoch，
丢掉任何一项都会让同名字段、共享引用或 reset 后复用的 handle 产生歧义。引用（`reference`）不借用 pickle 的
`call`——引用不是调用，混用会制造错误的危险信号。

`ac ed 00 05` 是完整的 stream header，加上严格消费，可靠到可以自动识别。

## 消费完整性

可往返的三个解析器有往返自检当运行时断言，解析器的 bug 会被它当场抓住；只读的两个没有这层保护。
替代物是**消费完整性**，两个格式判定方式不同，不能硬写成一条共同的结束 token：

- **pickle** 必须读到 `STOP`，`STOP` 后不能有剩余字节，且此时栈上**恰好一个值**。栈上多一个少一个都说明
  解析走偏了——这条比「读到了 `STOP`」强得多，能抓住 opcode 参数个数数错这类 bug。这比 `pickle.loads`
  严格（它读到 `STOP` 就返回，尾部垃圾不管；栈上剩两个值也照样返回栈顶），与 `pickletools.dis` 一致
  （它会报 `stack not empty after STOP`）。
- **Java 序列化流没有全局 `STOP`。** header 之后可以有零到多个顶层 content，解析器逐个解析直到输入 EOF。
  header-only 是空 stream，一个顶层值直接呈现，多个由 stream 节点保留顺序。剩余字节不能组成完整 content
  就报错，不能当尾部垃圾忽略。`TC_ENDBLOCKDATA` 只终止 class annotation、`SC_WRITE_METHOD` custom data 与
  protocol 2 Externalizable 数据，**不终止整个 stream**；这里的 EOF 是 Redis 值的边界，不是协议 token。

这些约束同时是自动识别的判据，所以宽松的解析器不只是漏 bug，还会直接毁掉自动识别。

## 资源耗尽

不实例化把风险从 RCE 降成了资源耗尽，但没有归零。pickle 尤其，因为它是台栈式虚拟机：

| 攻击面 | 后果 | 对策 |
|---|---|---|
| 声明巨大长度（`BINBYTES` 4 GB） | 预分配当场 OOM | **绝不按声明的长度预分配**，先校验剩余字节够不够 |
| `FRAME` 长度撒谎 | 越界读 / 解析错乱 | 校验 frame 不越过输入末尾 |
| 越界的 memo 编号 | `undefined` 混进树 | 取不到就抛错，不给默认值 |
| `MARK` 累积 | 栈无限增长 | `MARK` 层数上限 |
| `APPENDS` 反复放大 | 节点数爆炸 | 节点总数上限 |
| 深嵌套 | 下游递归遍历时炸栈 | 深度上限，见下 |

Java 一侧另有对象数组项数、handle 数、父类链长度上限；默认字段边界的回溯有两份**不随回溯恢复**的总预算
（handle 表快照的复制量、重复解析的字节数），避免特制输入把「候选数 × handle 数 / suffix 长度」放大。
各上限以 `pickle.ts` / `java.ts` 文件头的常量为准。

这些不是新发明：`php.ts` 的 `takeLength` 也是「声明的长度比整段数据还长就抛错」，深度上限五个解析器都有。外层还有
`MAX_CODEC_PAYLOAD_BYTES` 与 `FULL_VALUE_LIMIT` 限住输入大小。

**pickle 的深度上限性质与递归下降的解析器不同。** 那几个的上限挡的是它们自己炸栈；pickle 的解析器是
迭代的栈机（opcode 循环 + 显式栈），再深也不会栈溢出——这里的上限挡的是**下游**（`render.ts` 的递归渲染，
以及将来任何遍历这棵树的人）。所以上限要在**解析这一侧**报出来，否则「树太深」会变成某个下游的
`RangeError`；而那个检查函数（`assertDepth`）**自己也必须是迭代的**——防别人炸栈的函数不能先把自己栈溢出。

## 不做

- **`@msgpack/msgpack` 作运行时依赖。** 见「不用 @msgpack/msgpack 的五条理由」。
- **protobuf 的 wire 1 / 5 收成 `float`。** 见「wire 1 / 5 收成整数」。
- **JDK 集合类的 blockdata 读法清单**（内置 `HashMap` / `ArrayList` 的 `writeObject` 布局，把那几个 int 从 hex
  变成数字）。那是一张开放式的类特定知识表，跟「不列类型清单、只做机械验证」直接冲突，而收益只有几个
  int——内容本来就看得见。真要做也是独立特性，跟 `.proto` schema 导入同一类。
- **猜 Externalizable protocol 1 的长度**，或把剩余输入全部吞成 bytes。我们不加载类也不执行
  `readExternal`，不可能可靠定位后续 content，只能带原因拒绝。
- **pickle 自动识别放宽到 protocol 0 / 1。** 见「pickle」末段。
