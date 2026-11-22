# IPC 契约

覆盖：`src-tauri/src/commands/`、`src-tauri/src/services/values.rs`、`src-tauri/src/services/redis_values.rs`、`src-tauri/src/models.rs`、`src/api/`、`src/types/ipc.ts`、`src/lib/redisText.ts`

前端与 Rust 之间的边界是一组**按类型划分的语义化命令**：前端说「取这个 hash 的下一页」，
不说「发一条 `HSCAN`」。边界画在这里，是因为反过来的形状——前端拼命令、后端透传——
会把 Redis 协议知识全部泄漏进 Vue 组件：RESP2 / RESP3 的形状差异只能在组件里嗅探
（`ZRANGE … WITHSCORES` 回的是扁平数组还是成对数组，猜错就在半页处以为读完了），
三种分页语义被迫硬套成一种，逐条命令等于逐条 IPC，pipeline 无从谈起，
渲染进程里还得手搓并发队列，集群路由也只能交给 redis-rs 兜底。分层原则见
[architecture](../architecture.md)「前后端分层」，只读守卫见 [sessions](sessions.md)「只读属于会话」。

命令清单以 `src-tauri/src/commands/redis.rs` 的 `#[tauri::command]` 为准，前端封装在
`src/api/redis.ts`。TS 类型是 `src/types/ipc.ts` 手工维护的镜像，没有生成器——
改 `models.rs` 或 `services/values.rs` 的形状就要同步改它。

## 一个用户动作一个命令

命令层只做「一个用户动作 = 一个命令」的映射：**不做业务编排、不缓存、不推断意图。**
写侧最容易滑向 ORM，这条线就是防这个的。

- `redis_set_key_ttl` 用一个 `mode`（`TtlMode`）收敛 `PERSIST` / `EXPIRE` / `EXPIREAT`
  是可以的：它们是同一个用户动作（编辑 TTL）的三种表达。
- 「帮你算出该 `HSET` 哪些、该 `HDEL` 哪些」的 `save_hash(entries)` 不做：那是业务逻辑，属于前端。
- 一个命令内部可以发多批 pipeline。`redis_get_key_meta` 要两批——`TYPE` 决定了下一批
  用哪个大小命令——但对前端仍是一次调用。
- 「可能没有」用 `Option` 表达，不让前端 try/catch。pipeline 开了 `ignore_errors`，
  被 ACL 或 `rename-command` 拒掉的那一格读成 `None`（`redis_values::optional_value`），
  所以 `KeyMeta.memory` 为空是正常情况而不是错误；`slot` 只有集群才有。
- **批量是任务，不是命令。** 长耗时、可取消、带进度的操作（批量删除、批量 TTL、导入导出、
  克隆、内存分析）走 `services/task.rs` 的 `redis_start_*`。往命令层塞一个「批量」，
  这条线就松了。

## 参数形状按分页语义

参数按**每种类型真实的分页语义**设计，不做统一抽象：

| 类型 | 分页 | 语义 |
|---|---|---|
| Hash / Set | 游标 + MATCH + COUNT | 无序、可能重复；`COUNT` 是提示不是配额，一页回 0 条不代表结束 |
| List / ZSet | 下标区间（ZSet 另有 `rev`） | 有序、精确；后端带回 `total`，边界是算出来的 |
| Stream | ID 区间 + COUNT + `rev` | 开闭语义特殊，`XREVRANGE` 的端点反序由后端处理 |

顺序是 List 与 ZSet 的语义——插入顺序、分数序——SCAN 家族给不了，所以它们的主视图
只能按下标分页，也因此没有成员搜索。

## 写入参数是字节

写入命令的值参数一律是 base64 字节（`value_base64`、`field_base64`、`members_base64` 等）：
Redis 的值是任意字节，JSON 字符串装不下非 UTF-8。hash 字段名同理——拿界面上显示的
十六进制去 `HSET` / `HDEL`，写进去的是一个名叫 `1f 8b 08` 的新字段。

key 名例外：IPC 上是 `String`，只支持 UTF-8。SCAN 的一页里出现非 UTF-8 的 key 名，
整页报错（`connection::parse_scan_response`）。

## RedisText

取值命令返回的值一律是 `RedisText`（`models.rs`）：

- **`text` 与 `base64` 恰好有一个非空。** 字节是合法 UTF-8 时只给 `text`，否则只给 `base64`。
  UTF-8 无损可逆，前端用 `TextEncoder` 就能还原字节，不必每个值都多传一份 base64——
  一个 200 字段的普通 hash 全传两份纯属浪费。
- 消费方一律 `text ?? base64`：分支落在展示层（要文本还是要十六进制），而不是落在结构上。
  读取工具在 `src/lib/redisText.ts`。
- 非 UTF-8 不是边缘情况：gzip 的魔数是 `1f 8b`，**所有压缩值都走 `base64`**。

旧形状 `RedisValue`（UTF-8 转 `String`、否则转 `Object { type: "binary" }`，同一个字段两种形状）
只剩 CLI 的 `CommandResult` 在用，功能路径不再出现。

## 截断契约

`total_bytes` 非空即表示被截断，值是原始总字节数。

- 界面必须如实写出「显示前 N / 共 M」，不能默默给一个看起来完整的值。
- **截断过的值一律禁止保存。** 把截断后的内容写回去会静默丢掉后面的部分，这是最难发现、
  也最不可逆的一类数据损坏。编辑入口在 `total_bytes` 非空时必须转只读：`StringViewer`
  禁用保存；集合类的文本编辑框统一由 `lib/redisText.ts` 的 `textEditBlockedReason` 判断
  （截断或非 UTF-8 都禁用，后者在编辑框里是十六进制），Hash 与 List 共用它。
- **截断的 Set / ZSet 成员不能写也不能删。** 成员本身就是定位符，`ZADD` / `SREM` / `ZREM`
  都要完整字节；拿截断后的字节去发，`ZADD` 会新增一个成员，删除会删到恰好等于那段前缀的
  另一个成员。由 `memberLocateBlockedReason` 判断，界面禁用按钮并写明原因。
  非 UTF-8 但完整的 ZSet 成员只允许改分数，成员按原始字节写回。
- 截断由后端做，截的是原始字节。上限以 `redis_values::DEFAULT_VALUE_LIMIT`（STRING）、
  `values::MEMBER_VALUE_LIMIT`（集合成员）为准；成员的上限小得多，因为一页有几百条。

**STRING 的截断在服务端完成**（`values::windowed_string_value`）：`STRLEN` + `GETRANGE 0 (limit-1)`
一个 pipeline，一次往返拿到真实总长与窗口。整值 `GET` 会让服务端把值全量复制进这条连接的
输出缓冲区（512 MiB 的 key 就是瞬时多出 512 MiB 与一次可感知的停顿），再全量搬回来只为
本地截出一个窗口，传输还会顶穿响应超时。

pipeline 不是事务，两条命令之间值可能被并发写大。窗口取满而 `STRLEN` 对不上时一律按截断处理
（`redis_values::text_from_window`）——否则编辑器会放开一个其实被截断的值。

成员值（hash value、list item、set / zset member）没有范围读命令，只能取回后在后端截。

截断点可能落在多字节字符中间，这时退到最后一个完整字符（`redis_values::decode_utf8`）——
否则一段纯中文会因为切在汉字腰上而整个退化成 base64。

## 完整值重取

`redis_get_full_value` 按定位符（`values::ValueRef`）把分页里被截断的那份值重取回来，
上限以 `values::FULL_VALUE_LIMIT` 为准。它存在的理由是解码：半截 gzip 跟半截 msgpack 一样解不开，
想解码就得先有完整字节（见 [frontend/codecs](../frontend/codecs.md)「截断守卫」）；而成员值的分页上限
很容易撞到——一个 Spring 的 Java 对象、一份 PHP session、一条 msgpack 缓存都可能超。

- 它是「一个用户动作 = 一个命令」：用户在 `ValueDrawer` 的截断横幅里点「加载完整值」。
  **必须显式触发**，不是把分页上限放大——一页 200 条各取完整值，每次打开 key 都是灾难。
  点 codec 不会触发重取：截断的值上 codec 菜单直接置灰，下一步由横幅给出。
- 返回形状恒定是 `RedisText`，`ValueRef` 是同一个语义（按定位符取单个值）的几种定位形态，
  是真正的 sum type，不是下面「不做」里 `get_key_detail` 的复活——那个撑不住是因为它把三种
  不同的分页语义塞进一个签名，参数互斥、返回形状随类型变。
- `FULL_VALUE_LIMIT` 比 `codec::MAX_CODEC_PAYLOAD_BYTES` 小：值要 base64 过 IPC，膨胀三分之一，
  解码上限那么大的值过 IPC 会卡。
- 截断契约不变：重取后 `total_bytes` 为空才允许保存，仍然超限就仍然只读。

各类型能不能重取，取决于它有没有一个**不截断的定位符**：

| 类型 | 定位符 | 重取 |
|---|---|---|
| String | key | 走与 `redis_get_string_value` 同一条窗口路径，只是上限更大 |
| Hash | key + field | `HGET`。field 名在分页里不截断（`values::short_text`），按原始字节发 |
| List | key + 下标 | `LINDEX` |
| Set / ZSet | 成员本身就是值 | **不能** |

**Set / ZSet 重取不了，这是 Redis 的数据模型。** 成员既是值又是定位符：要精确定位它得先有完整成员，
而正是因为它被截断了才要重取。所以没有对应的 `ValueRef` 变体，`SetViewer` / `ZSetViewer` 不给抽屉传
定位符，而是传 `noTargetReason`，横幅上照实写出原因，而不是把按钮一禁了事。谁想「补齐」这一格，
先看这一段。

调用方只有 `ValueDrawer`：`HashViewer` 传 `hashField`，`ListViewer` 传 `listIndex`。`string` 变体后端
可用，但目前没有前端调用方（`StringViewer` 的截断横幅不提供重取），见 [backlog](../backlog.md)
「截断的 STRING 解不了码」。Stream 没有变体，见 [backlog](../backlog.md)「Stream 里的字段值解不了码」。

## 不做

- **通用的「发一条命令」入口**（`runCommand`、结构化的 `redis_execute_command`）。有了它，
  整个应用的功能路径迟早都从那里走，后端退化成透传管道，开头那一串问题全部回来；
  只读守卫也会因为同时要管 CLI 与功能路径而站错位置。CLI 的唯一入口是 `redis_execute_cli`，
  整行由后端解析（`services/cli.rs`）——留一个「方便时可以绕过分层」的后门，分层迟早会被绕。
- **`redis_get_key_detail`：一个命令返回整个 key 的值。** 它的签名撑不起来：六种类型的分页语义
  要塞进一个签名，Hash / Set 要游标、List / ZSet 要下标、Stream 要 ID 区间，参数退化成一个
  大 union，返回的 cursor 每种类型含义还不一样。补全它只会得到一个谁都不想用的命令，
  所以拆成按类型的取值命令。
- **算 diff 的保存命令**，见「一个用户动作一个命令」。
