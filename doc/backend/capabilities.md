# 服务端能力

覆盖：`src-tauri/src/services/capabilities.rs`、`src-tauri/src/services/connection.rs`（`probe_server`）、`src-tauri/src/state.rs`（`RedisSessionRuntime.capabilities`、`capability_sink`）、`src-tauri/src/error.rs`（`PermissionDenied`、`CapabilityUnsupported`、`AppError::classify`）、`src-tauri/src/services/values.rs`（`set_string_value`）、`src/lib/capabilities.ts`、`src/stores/sessions.ts`（`applyCapabilityChange`）、`src/api/events.ts`（`onCapabilityChange`）、`src-tauri/tests/redis_capabilities.rs`

一条规矩：**能力不足要在界面上说，不要等用户点下去才报错。** 要说得出，就得先知道服务端会什么。
这里定的是：能力从哪里来、几种状态、未知时怎么办、权限算不算能力。

不按 `redis_version` 判断，因为它在兼容实现上会判反：Valkey 8 起它固定报 `7.2.4`，真实版本在
`valkey_version`；Dragonfly、KeyDB 报的是兼容声明，不是能力集。而且一半的能力是参数级的
（`SET KEEPTTL`、`EXPIRE NX`），只问「命令在不在」不够；模块能力（JSON、Time Series）看版本号
也得不出结论，只能问服务端。

## 三个维度，不要混在一起

| 维度 | 问题 | 来源 | 何时确定 |
|---|---|---|---|
| **身份** | 这是什么实现、什么版本 | `INFO server` | 建连时 |
| **能力** | 服务端会不会这条命令 / 这个参数 | `COMMAND INFO` + 静态注册表 | 建连时，运行时可降级 |
| **权限** | 当前用户能不能执行 | **不预检**，执行时的拒绝 | 每次执行 |

能力和权限是两件事。`COMMAND INFO` 通常是放行的，即使用户执行不了它查到的那条命令；反过来
`COMMAND` 被禁也不代表服务端不支持什么。所以 ACL 禁了 `EXPIRE` 的用户，`expireConditions` 照样是
supported——执行时报的是权限错误，不是「不支持」。

## 身份

`ConnectionOpened.server` 是 `ServerIdentity`：`flavor`、`version`（实现自己的版本）、
`compatVersion`（`redis_version` 原值，只用于展示与排障）。判定按字段，不按版本号猜，
判据是在真实服务端上实测的（`tests/redis_capabilities.rs`），以 `ServerIdentity::parse` 为准：

| 实现 | 判据 | `version` 取自 |
|---|---|---|
| Valkey | `server_name:valkey` 或存在 `valkey_version` | `valkey_version` |
| Dragonfly | 存在 `dragonfly_version` | `dragonfly_version`，剥掉 `df-v` 前缀 |
| KeyDB | `INFO server` 里有 `features` 行 | `redis_version`（KeyDB 的版本就写在这里） |
| Redis | 以上都不命中且有 `redis_version` | `redis_version` |
| unknown | `INFO` 被拒或没有任何版本字段 | — |

KeyDB 在 `INFO server` 里唯一的特有字段是 `features`。没有这一行的老 KeyDB 会被认成 Redis 6.x——
它本来就是 Redis 6 的分支，能力结论一致。Redis Stack 不是独立 flavor：它是 Redis 加模块，
模块能力走命令级判据。

界面展示一律写实现自己的名字和版本（「Valkey 8.1.1」），不写 `redis_version`。后端生成的文案用
`ServerIdentity::label`，兼容声明与真实版本不同时附在括号里。

## 注册表

`services/capabilities.rs` 的 `REGISTRY` 是一张**静态**表，每一项声明判据、文案里的功能名、
未知时的策略。现有的项与它们的用户以 `REGISTRY` 为准。判据两种：

- **`Command(name)`**：`COMMAND INFO` 里存在即支持。
- **`Argument { command, since }`**：命令存在，且该实现的版本不低于 `since` 里给它登记的值。
  参数级能力几乎都是「某版本起」，按 flavor 列最低版本足够，且可审阅。
  - **`since` 里没登记的 flavor 得 unknown，不得 supported。** 没把握就不填，不要硬编一个版本号——
    实测某个版本支持，也不等于知道最低版本，填进去就会把更老的版本错判成「需要 x.y 及以上」。
  - `COMMAND INFO` 只能把结论往「不支持」推：命令被改名了，参数再新也发不出去。它拿不到时不改变结论，
    判据仍是版本号。

**每一项随它的第一个用户一起登记**，不预先登记没有用户的能力——没人用的条目没人验证，迟早写错。
`CapabilityId` 在 Rust 是 `serde(rename_all = "camelCase")` 的枚举，TS 侧是 `ipc.ts` 的
`CAPABILITY_IDS`（`as const` 对象，`erasableSyntaxOnly` 禁 enum），两边手改。

## 三态与 `usable`

每一项是 `Capability { state, usable, reason }`：

- `state`：`supported` / `unsupported` / `unknown`。
- `usable`：按该项「未知时」策略折算后的结论。**界面只认它。** unknown 算能用还是不能用每一项不同，
  策略只写在后端注册表一处；前端再判一遍就是第二份实现。
- `reason`：后端生成，界面原样显示。判断依据只有后端知道——前端自己拼「该服务端是 Redis x」在
  Valkey 上就是错的。

**未知时的策略**判据只有一条：**不支持时会不会静默做错事。** 会，就禁用或走降级路径；只会大声报错，
就可以允许。不一刀切置灰：一律禁用会把认不出的兼容实现上的基础功能整个挡死。

- `expireConditions` 未知时禁用：挡住的代价只是少一个选项。
- `setKeepTtl` 未知时允许：它是 STRING 保存的基础路径，服务端不认 `KEEPTTL` 时报语法错误，
  不会静默清掉 TTL（并且会触发运行时降级）。
- `scripting` 未知时允许：新建、克隆、List 按下标删除都靠脚本，被拒时 `EVAL` 会大声报错。

## 探测

- **建连时一个 pipeline，不增加往返**（`connection::probe_server`）：`INFO server` +
  `COMMAND INFO c1 … cN`，名单是注册表引用到的命令（`probed_commands`），静态、有限，
  回复体积有上界。回复里是 nil 的命令记为不存在。
- **探测失败绝不让连接失败。** `INFO` / `COMMAND` 会被 ACL 或 `rename-command` 拒掉，那不是连不上的理由；
  拿不到的部分落成 unknown，交给各项的未知时策略。`COMMAND` 被 ACL 拒绝与其他原因拿不到在 `reason`
  里分开说。
- **`rename-command` 改名的命令查回 nil**，记为 unsupported，`reason` 写「不存在或已被改名」。
  按原名发不出去，这个结论对功能路径是对的。
- **会话级，不跨会话缓存。** 能力表放在 `RedisSessionRuntime.capabilities`。切 DB 等于新会话，会多探测一次；
  换来的是服务端升级、改完配置作废会话之后不会读到陈旧能力。
- **集群只探测一个主节点**（`PipelineTarget::AnyNode`），Sentinel 探测解析出的 master。滚动升级期间
  节点能力可能不一致，为此逐个探测所有 master 不值得，交给运行时降级兜底。
- 克隆的 transfer runtime 也会探测，但它的能力表随用随弃。克隆入口查的是用户的**浏览会话**
  （`clone_task::prepare`）。

## 功能入口守卫

前端按 `usable` 置灰并显示 `reason`；后端在入口用 `capabilities::require` 守同一条规则——
前端漏了一处，后端也不会把一个服务端不认的东西发出去。现有的守卫点：

| 能力 | 后端入口 | 界面 |
|---|---|---|
| `expireConditions` | `start_batch_ttl_task`（带条件时，试算也拦） | `BatchTtlDialog` 条件区置灰，提示用 `reason` |
| `scripting` | `create_key`、`delete_list_item`、克隆的 `prepare`（源与目标） | `KeyWriteDialog` 横幅与提交按钮；`ListViewer` 删除按钮 |
| `setKeepTtl` | `set_string_value`，见下一节 | `StringViewer` 保存按钮（仅在它与 `scripting` 都不可用时置灰） |

前端的判断走 `lib/capabilities.ts` 的 `capabilityBlockedReason`。

## STRING 保存

保存只更新值、原样保留过期时间——裸 `SET` 会清掉 TTL。`set_string_value` 按 `setKeepTtl` 走三条路径：

| 状态 | 做法 |
|---|---|
| `supported` | `SET key value KEEPTTL` |
| `unknown` | 同上；服务端报错时运行时降级 |
| `unsupported` | `SET_KEEP_TTL_SCRIPT`：`PTTL` → `SET` → 有 TTL 则 `PEXPIRE`，原子执行 |

降级路径必须是脚本：分两条发会在 `SET` 与 `PEXPIRE` 之间留下永久 key 的窗口，同
`DELETE_LIST_ITEM_SCRIPT` 不拆成 `LSET` + `LREM` 的理由。如果 `scripting` 也不可用，明确拒绝保存并说明原因——
**绝不退回裸 `SET` 悄悄清掉 TTL。**

## 运行时降级

受能力保护的命令如果被服务端以「不认识」拒绝，说明建连时的判断错了（判据写错、集群节点不一致、
兼容实现谎报）。`capabilities::guard` 包住这些命令的结果：

1. 本次操作按失败报告（`redis.capability_unsupported`），文案说明是能力问题而不是数据问题；
2. 会话内把该项降为 unsupported，经 `state.capability_sink` 推到 `redis://capability-change`，
   前端 `applyCapabilityChange` 更新会话，界面随之置灰或换路径。

**不自动重试，也不自动切换到降级路径重做一遍**——用户点的是保存，换一种方式悄悄执行不是他确认过的东西。
下一次操作才按降级后的状态走（STRING 保存因此改走脚本）。

只认「不认识」这一类回复（`looks_unsupported`）：命令级能力只认 `unknown command`；参数级另认
`syntax error` 与 `wrong number of arguments`。脚本里的业务错误、权限拒绝、`WRONGTYPE` 都不算——
误降级会把一个可用的功能在整个会话里置灰。脚本内调用了服务端没有的命令，回的是
`Unknown Redis command called from script`，同样不算 `scripting` 不支持。

批量 TTL 带条件时，一批回复里只要有一格是「不认识这个参数」，`apply_ttl_batch` 就把它当成整批失败交给
`guard`，而不是记成一串 failed——判断错了的话整批都会同样失败。

`capability_sink` 由 `lib.rs` 的 `setup` 注入；无窗口的集成测试可以注入自己的 sink 来断言事件，
不注入时降级照常发生、只是不推送（同 `clone_task::CloneEventSink` 的做法）。

## 权限错误

权限拒绝映射成 `AppError::PermissionDenied`（`redis.permission_denied`），文案「当前用户无权执行 `EXPIRE`」，
原始回复放在 `detail`。和「不支持」分开说，因为两者的处理人不同：不支持是换服务端或升级，
没权限是找管理员改 ACL。

权限没有便宜可靠的预检手段（`ACL DRYRUN`、`ACL GETUSER` 本身要管理权限），所以只在执行时报。
分类集中在 `AppError::from_redis` / `from_server_error`，executor 的错误都经过它。拒绝有三种形状，
后两种错误码是 `ERR`，只能按正文认（以 `AppError::classify` 的表为准）：

- 直接调用：`NOPERM`，正文里有命令名；
- 脚本内，Redis 7 / Valkey：`ACL failure in script`，正文里是**脚本调用的**命令名，不是 `EVAL`；
- 脚本内，Redis 6：没有命令名。此时不点名（「当前用户无权执行脚本里调用的命令」），不能错点成 `EVAL`。

## 新增一项能力

1. `CapabilityId` 加一个变体，`ipc.ts` 的 `CAPABILITY_IDS` 同步；
2. `REGISTRY` 加一项：判据、功能名、未知时策略（按「会不会静默做错事」定）；`Argument` 的 `since`
   只填有把握的 flavor；
3. 在功能入口 `require`、在受保护的命令上 `guard`，界面按 `usable` 置灰；
4. `tests/redis_capabilities.rs` 补该项在矩阵上的结论。e2e 矩阵（Redis 5 / 6 / 7 / 集群、Valkey 8 / 9、
   KeyDB、Dragonfly）由 `scripts/dev/e2e.ps1` 起。

## 不做

- **用 `COMMAND DOCS` 判参数级能力。** 它带参数与 `since`，但 7.0 才有，兼容实现的支持程度不一，
  回复体积也大一个量级。
- **权限预检**（`ACL DRYRUN` / `ACL GETUSER`）：本身要管理权限，见「权限错误」。
- **跨会话缓存能力**，见「探测」。
- **逐个探测集群所有 master**，见「探测」。
- **被拒后自动重试或换路径重做**，见「运行时降级」。
- **预先登记没有用户的能力。** Hash 字段 TTL（`HEXPIRE`）随它的功能一起登记，见 [backlog](../backlog.md)
  「Redis 8 新类型与能力」。
- **给实测过的实现补 `since`。** Dragonfly 实测支持 `EXPIRE NX` 与 `KEEPTTL`，KeyDB 6.3 实测支持 `KEEPTTL`，
  但最低版本不知道，所以它们在 `since` 里留空、得 unknown。
