# 编解码器：归属与边界

覆盖：`src-tauri/src/services/codec.rs`、`src-tauri/src/commands/native.rs`（`native_codec_*`）

谁来解码、解到什么程度、解不开怎么办。渲染进程一侧的框架见
[frontend/codecs](../frontend/codecs.md)，各格式的事实见 [frontend/codec-formats](../frontend/codec-formats.md)。

这里的多数结论会被「顺手优化」掉，除非写下理由——尤其「Readonly 安全边界」那一节，
它看起来像一个待办项，实际上是不能做的。

## 解决什么

Redis 存的是字节，但应用几乎不写明文：

- **压缩**——5 KB 的 JSON gzip 后 800 字节，缓存场景很多框架默认开。
- **序列化**——更普遍。Spring 的 `RedisTemplate` **默认**用 `JdkSerializationRedisSerializer`，
  所以随便一个 Java 团队的 Redis 里，值全是 `ac ed 00 05` 开头的字节流。PHP 的 session handler
  用 `serialize()`，Python 用 pickle，跨语言的用 msgpack / protobuf。

用任何 GUI 打开这些 key，看到的都是乱码。codec 就是认出这是什么、解开给用户看。
**这是这类工具的核心价值之一，不是边角功能。**

## 真实分布决定配置形状

1. **同一个应用写的 key 编码一致**——编码在序列化器层面配置，不是逐 key 决定。
2. **同一个实例可能有多个应用**——`cache:*` 是 gzip + JSON，`session:*` 是 PHP serialize，
   `counter:*` 是明文数字。
3. **同一个应用里 key 与 value 的编码常常不同**——Spring 的典型配置是 key 用
   `StringRedisSerializer`（明文）、value 用 JDK 序列化（二进制）。

第 3 条直接否掉「连接级一刀切」：对 key 也应用 codec 会把 key 名解坏。

## 归属切分

归属以 `codec::codec_support_matrix()` 为准，前端镜像是 `src/lib/codecs.ts` 的 `CODECS`
（一一对应，没有生成器）。切分线是：

> **字节 → 字节的变换归后端；字节 → 结构的变换归前端。**

| `CodecConclusion` | 性质 | 实现 |
|---|---|---|
| `RustRoundTrip` | 压缩：字节 → 字节，可逆 | `services/codec.rs` |
| `RendererRoundTrip` | 序列化：字节 → 结构，可逆 | `src/lib/codec/` 的 `RENDERER_CODECS` |
| `Readonly` | 序列化：字节 → 结构，只解不编 | `src/lib/codec/` 的 `READONLY_CODECS`，见「Readonly 安全边界」 |
| `DeferredUnsupported` | 没有实现 | — |

前者是传输层问题：无歧义、可逆、吃 CPU，Rust 的主场。后者是展示层问题：解出来直接进渲染层，
中间任何一次 JSON 化都有损——msgpack 的 int64 精度、binary 字段、非字符串 map key 会全丢；
protobuf 无 schema 时「怎么呈现」本身就是展示决策，塞进 Rust 等于把 UI 逻辑写进后端。
这条理由有一个实现约束：前端的产物必须是保留类型的结构，不能再 JSON 化回 `RedisText`，
见 [frontend/codecs](../frontend/codecs.md)「产物是结构不是文本」。

`native_codec_decode` / `native_codec_encode` 的输入输出都是**字节**（`CodecDecodeResult.value`
是 `RedisText`，encode 的输入是 base64），不是文本：解压的产物未必是文本，gzip 里裹一层
msgpack 在 PHP / Python 应用里很常见，链式解码就靠这一点。后端只认四种压缩算法，其余
codec 一律回 `codec.unsupported`；`detect` 会跳过它们。

界面照实区分「声明能力」与「当前可用」：未实现的 codec 在 `CodecPicker` 里置灰并注明原因、
在设置页标「未实现」（`lib/codec/index.ts` 的 `isImplemented` / `unimplementedReason`），
不等用户点下去才报错。

### 只有后端解码才能做智能截断

一个 10 MB 的值不可能整个渲染，必须截断，**而不解码就不知道该从哪截。**

如果压缩类的解码在前端，后端就必须把完整的压缩字节传过去——截断压缩字节等于毁掉它，
前端拿到半截 gzip 什么也解不出来。也就是说解码在前端 = 大 value 必须完整过 IPC。
解码在后端则可以只传一个窗口加总长度，这就是 [ipc-contract](ipc-contract.md)「截断契约」里
`total_bytes` 的来历。

`codec::decode` 本身不截断：截断由取值命令负责，解码是用户显式发起的一次性动作，两处都截会让
`totalBytes` 的含义变得含糊。输入与解压产物各受 `MAX_CODEC_PAYLOAD_BYTES` 约束（解压炸弹）。

## 三层配置

三层叠加，不是三选一：

1. **自动识别**——`CodecPicker` 菜单里的「自动识别」，覆盖有可靠魔数的格式（见下一节）。
   由用户点击触发，打开 key 时不会自动跑。
2. **连接级默认**——识别不了时兜底，**只作用于 value，不作用于 key**。这一层**不存在**：
   `ConnectionConfig` 里没有 codec 字段，见 [backlog](../backlog.md)「msgpack 与 protobuf 只能一个一个手点」。
3. **单 key 临时覆盖**——`CodecPicker` 里手点某个 codec，排查时用。

## 自动识别的边界

靠魔数识别，可靠性差别很大：

| 可靠 | 魔数 | 不可靠 | 原因 |
|---|---|---|---|
| gzip | `1f 8b` | brotli、raw deflate | 无魔数，只能试解 |
| zlib | `78 01` / `78 9c` / `78 da` | msgpack | 无魔数，任意字节都可能是合法 msgpack |
| Java 序列化 | `ac ed 00 05` | protobuf | 无魔数，且无 `.proto` 时只能解出字段号 |
| PHP serialize | `a:` / `O:` / `s:` … | | |
| pickle（protocol 2+） | `80 02` … `80 05` | | |

**最常见的两种（gzip、Java 序列化）都有可靠魔数**，所以自动识别能覆盖大部分。
msgpack / protobuf 永远不能进候选集，它们本该由第 2 层兜底。哪些 codec 进候选、识别分几趟，
见 [frontend/codecs](../frontend/codecs.md)「识别的三个维度」。

## 硬规则

不写死的话，后面一定会变成 bug：

- **解码失败不妨碍看值。** 解不开时链不前进，屏幕上仍是上一层的字节，失败原因以通知说出来
  （`CodecPicker` 的 `decode`）。将来的连接级 codec 也必须如此：一个配错的默认 codec 如果让取值
  失败，整个 key 浏览器就瘫了。回退要如实说明，而不是假装无事发生。
- **key 永远不解码。** 见「真实分布决定配置形状」第 3 条。而且 key 是路径的一部分（尾随空格都是
  真实字节），解码它会破坏树结构与 namespace 切分。
- **导出导原始字节。** 导出是为了备份 / 迁移，解码是展示行为，混淆两者会导出一份「看起来对但
  导不回去」的数据。`redis_start_export_keys` 走 `DUMP`，`ExportKeysTaskOptions` 不接 codec 参数。
- **截断过的值不解码。** 半截 msgpack 跟半截 gzip 一样解不开——`totalBytes` 非空时直接拒绝并说明
  原因，而不是解出一个残缺结构。压缩类与序列化类共用同一个守卫，论证见
  [frontend/codecs](../frontend/codecs.md)「截断守卫」。

## Readonly 安全边界

**pickle 与 Java 序列化不能做成 RoundTrip。** 禁的是**实例化与编码写回**，不是解析。

用语言运行时的常规反序列化 API 加载这两个格式，会执行 pickle 的 `__reduce__` 或 Java 的
`readObject` 等钩子。查看器若加载类并实例化对象，攻击者只要往 Redis 里写一个构造过的 payload，
用户打开那个 key 就可能中招。所以实现走**不加载类、不实例化对象的纯结构解析**：只按字节协议
解出类名、字段与原始数据，不调用构造函数或反序列化钩子。解析本身不是执行。

**只解析不执行的查看器，是这两个格式唯一安全的查看方式。** 用户想看一个 pickle 值，替代方案是
自己起个 Python `pickle.loads()`——那才是真中招。做这个功能是在**降低**用户的风险；解出来能看见
`GLOBAL os system` + `REDUCE` 这样的调用点，本工具顺手成了 payload 检查器。把它藏起来才是危险的。

不能 encode 的理由有两条，**两条的并集才是这条边界的真实形状**：

1. **写回就是在制造可执行载荷。** pickle 的 `GLOBAL` + `REDUCE` 是「加载时调用什么」的字面表达。
   一个能编辑 `GLOBAL` 模块名再存回 Redis 的编辑器，等于一个构造 RCE payload 的工具，而这个
   payload 会被**别人的**应用加载。解析的风险由本工具承担且可控（见
   [frontend/codec-formats](../frontend/codec-formats.md)「资源耗尽」），写入的风险落在下游，
   我们既控制不了也看不见。**这条跟往返保真度无关**：就算能一字节不差地重编，也不该提供这条路。
2. **就算不谈安全，往返也不可靠。** pickle 的 memo 编号依赖序列化时的对象共享关系，与 PHP 的
   `R:` / `r:` 同构，结构一改编号就指错。Java 的 `writeObject` 写出的自定义数据不自描述，改了之后
   长度、校验、偏移还成不成立我们不知道，与 PHP 的 `C:` 同一类。

只拿「编码需要知道原始类的语义」当理由是不够的：它对 Java 成立，对 pickle 不成立——pickle
完全自描述，技术上重编得出来。照那条理由推，pickle 就该是 `RendererRoundTrip`。

`CodecConclusion::Readonly` 记的就是这件事：「可安全查看、永不写回」，不是 `DeferredUnsupported`
的别名——真是禁区的话直接叫 Forbidden 就好，这个值就没有存在的意义了。前端用单独的接口与
注册表承载它，见 [frontend/codecs](../frontend/codecs.md)「ReadonlyCodec」。

不实例化确实挡掉了主要风险：Java 经典的 SerialDOS（嵌套 `HashSet` 的 `hashCode` 爆炸）在**实例化时**
才触发，纯解析根本不碰。剩下的攻击面是资源耗尽，不是代码执行。

PHP 的 `O:` 是不同性质：PHP 的危险在 PHP 运行时的 `__wakeup` / `__destruct`，格式本身是纯数据且
自描述。我们只解析不构造，所以 phpSerialize 是 `RendererRoundTrip` 而不是 `Readonly`。

## 不做

- **把 `Readonly` 挪进 RoundTrip、给 pickle / Java 补 `encode`。** 先逐条驳上面两条理由。
- **全放后端。** 形状最简单，后端任务管线能复用；但结构化格式转 JSON 会丢类型，protobuf 的
  呈现决策被塞进 Rust，切 codec 要走一趟 IPC，Rust 侧 msgpack / php-serialize 生态弱，而且每加一个
  crate 都要进许可清单。
- **全放前端。** 切 codec 零延迟、结构不丢类型、JS 生态成熟；但原始字节 base64 后膨胀 33%，
  brotli 在 webview 里没有原生 API（`DecompressionStream` 只有 gzip / deflate）要引 wasm，
  大 payload 解压阻塞渲染线程，后端任务管线彻底看不到解码后的值，截断也只能在压缩字节上做
  （见「只有后端解码才能做智能截断」）。
