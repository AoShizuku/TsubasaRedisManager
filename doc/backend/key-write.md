# 新建与克隆

覆盖：`src-tauri/src/services/key_write.rs`、`src-tauri/src/services/clone_task.rs`、`src-tauri/src/services/connection.rs`（`open_transfer_runtime`）、`src-tauri/src/services/task.rs`（`cancel_task` 的克隆分支）、`src-tauri/src/models.rs`（`ClonePhase`、`CloneTaskSnapshot`、`TaskEventItem::Clone`）

两条写路径，共同的底线是**不覆盖**、**不经过查看器**。对话框与克隆结果的消费见
[frontend/key-write-dialog](../frontend/key-write-dialog.md)。

## 命令形状

命令以 `commands/redis.rs` 为准，`values.rs` 导出入口，编排在 `key_write.rs` 与 `clone_task.rs`。不提供裸命令入口。

- `CreateKeyValue` 按六种类型分支，字段与成员一律 base64 字节；创建返回写入的 `keyType`。
- `KeyExpiry` 有四种 mode：`persist` / `preserve` / `relative` / `absolute`，后两者携带毫秒数。
  **`preserve` 只允许克隆使用**（`KeyExpiry::args` 的 `cloning`）——新建没有「源」可保留。
- 克隆是任务：`redis_start_clone_key` 注册任务、立即返回 task id，`redis_get_clone_task` 给权威快照
  （见「任务状态与取消」）。**克隆没有同步的 Tauri 命令**：同步入口会绕开取消与 `uncertain`
  终态这些任务语义。服务层的 `key_write::clone_key` 保留为同步入口，跑同一个 `Run::transfer`
  （`clone_inline`），只给 e2e 用。
- `redis_key_exists` 是严格的 `EXISTS`，供界面在结果不确定时核对，ACL / 通信错误照实返回。

## 创建原子性

`create_key` 只发一条 `EVAL`，脚本（`CREATE_SCRIPT`）只访问目标这一个 Key：

1. 后端先完整解码并验证：行数（`MAX_ROWS`）、总字节（`MAX_INITIAL_BYTES`）、重复项、score
   （`initial_args`）。任何一项不过，什么都不发。
2. 脚本检查目标不存在；按服务器 `TIME` 把相对时间换算成绝对时刻，并验证它晚于服务器当前时间。
3. 若有 TTL，先在这个**不存在的** Key 上执行一次 `PEXPIREAT`。此时没有写入副作用，
   但已经验证了命令可用性、参数和 ACL。
4. 一条 `SET` / `HSET` / `RPUSH` / `SADD` / `ZADD` / `XADD` 写完全部初始内容，再设置 TTL。

**Lua 没有错误回滚**，不能把「用了 Lua」当作回滚保证：脚本中途报错，之前执行过的写命令照样生效。
所以原子性要靠命令的排列来挣：

- 第 3 步特意排除了「内容创建成功、TTL 因 ACL 失败，留下一个永久 Key」。同一次脚本执行中 ACL
  不会被别的客户端改变，第 3 步过了，第 4 步的 `PEXPIREAT` 就不会因权限失败。
- 每种类型只有**一条**初始内容命令，不在循环中逐条写成员——否则中途某条失败会留下半个集合。
- 初始内容写入失败时目标仍不存在，**不需要依赖 `DEL` 清理**——`DEL` 可能同样被 ACL 禁止。

e2e：`tests/redis_key_write.rs` 的 `denied_ttl_does_not_leave_a_permanent_key_even_when_del_is_denied`、
`creation_races_and_validation_do_not_overwrite_or_leave_partial_keys`。

## 克隆：原生快照

统一使用 `DUMP` / `RESTORE`，**不经过查看器、codec、分页、截断或任何有损转换**，所以超过查看器窗口的值
也完整复制。源与目标各按单 Key 路由（单 key 脚本、`PipelineTarget::Key`），因此支持跨连接、跨 DB、
集群跨槽位，以及单机与集群之间复制。

- **自我克隆**：同一服务器配置（`same_server`）、同 DB、同名时直接拒绝（`invalid_target`）。
  不同别名实际指向同一个 Key 时 `same_server` 认不出来，由目标的同名保护拒绝。
- **源**：`SNAPSHOT_SCRIPT` 一次取得 `TYPE`、`DUMP`、`PTTL`、`TIME`，快照与过期状态原子一致。
  脚本只读，所以只读会话可以做源。源在快照之后的变动不追溯到副本——克隆不锁源直到目标写完。
- **目标探测**：先 `COMMAND INFO RESTORE`。明确回空表示不支持，停止；ACL 拒绝探测只意味着**未知**，
  交给真实的 `RESTORE` 校验。再用一条单 key 脚本检查同名并取目标 `TIME`（供「跨服务器 TTL」校准）。
- **目标写入**：`RESTORE_SCRIPT` 一次完成到期检查与 `RESTORE … ABSTTL`。**所有写入都不带 `REPLACE`**：
  探测之后才出现的并发同名，仍由 `RESTORE` 原子拒绝（`BUSYKEY` → `redis.key_exists`）。
- **格式不兼容**：目标收到序列化数据后自己检查 RDB 版本、校验和与模块格式。被拒时报告
  `redis.key_incompatible`（`restore_error`），**不尝试重建成普通文本**——那是有损的，而且对模块类型无从重建。

## 专用连接与后台传输

- **目标必须是可写会话**（`prepare` 判目标会话的 `readonly`）；源不判。见 [sessions](sessions.md)「只读属于会话」。
- 启动时捕获两个会话的连接配置，任务用 `connection::open_transfer_runtime` 单独建立连接与所需的
  TLS / SSH / 哨兵 / 集群资源，**不占用浏览会话的 executor**（[sessions](sessions.md)「专用连接」）。
  关闭对话框或普通会话不会丢弃已启动的克隆；任务持有自己的资源，结束时释放。
- 普通会话的响应超时是 `REDIS_RESPONSE_TIMEOUT`；传输连接的响应超时与每个阶段的等待上限是 5 分钟
  （以 `STAGE_TIMEOUT` 与 `open_transfer_runtime` 为准）。一个几十 MiB 的 `DUMP` 跨 WAN 搬运，
  装不进交互命令的预算。
- **全应用只允许一个克隆传输在途**（`TRANSFER_SLOT`），其余在 `queued` 排队。`DUMP` 不能流式读取，
  每个在途克隆都要在内存里持有整份序列化数据，并发几个大值就能把内存撑爆。

## 跨服务器 TTL

同一服务器配置（`same_server`）沿用源的绝对到期时刻。跨服务器不能直接照搬源时间戳：目标的墙钟
可能快或慢数分钟。所以先在目标 Key 所在节点取 `TIME`，记其回复接收时的单调时刻 B；
再在源请求发出时记单调时刻 S，从原子快照取得剩余毫秒数 R：

> 目标到期时刻 = 目标 TIME + (S − B) + R

实现在 `translated_deadline`。

- **B 取接收时刻、S 取发出时刻，是保守边界**；客户端自身的墙钟不参与计算。
- 这是一个**固定的绝对期限**：读取快照、排队和上传自然消耗 TTL，不从 `RESTORE` 时重新开始倒计时。
  网络往返可能让副本略早过期，不把这些延迟加回有效期——宁可早过期，不可让副本比源活得久。
- 永久源仍为永久。目标开始恢复时期限已到则停止（`SOURCE_EXPIRED`），**不复活已经过期的快照**。
- 用户选择自定义相对时间时，才从目标写入那一刻开始计时（`RESTORE_SCRIPT` 的 `relative` 分支）。

该校准只处理 **Key 级 TTL**。数据类型内部的绝对时间（例如 Hash 字段 TTL）遵循 Redis 原生
`DUMP` / `RESTORE` 语义，不重写其二进制结构。同一集群内沿用绝对期限，依赖集群节点时钟同步；
跨连接校准假设单次传输期间服务器时钟没有大幅跳变。

## 任务状态与取消

阶段以 `ClonePhase` 为准：`queued → connecting → reading → restoring → completed`，
异常终态是 `cancelled` / `failed` / `uncertain`。

- 进度走既有的 `redis://task-progress` 频道，条目是 `TaskEventItem::Clone`，带 `CloneTaskSnapshot`
  （源与目标、阶段、已获取的序列化大小、类型）。`DUMP` 没有分块进度，**不编造传输百分比**：
  通用计数上 `total` 恒为 1，只在终态记一次（`Run::report` 的映射；`uncertain` 记作失败）。
  通用计数的口径见 [tasks](tasks.md)「进度口径」。
- 克隆详情另存一张表（`AppState::clone_tasks`），`redis_get_clone_task` 读它，作为权威快照，
  前端靠它兜底事件丢失与「事件早于订阅」的竞态。

**取消与进入提交在任务注册表的同一把锁下裁决**：`Run::commit` 在 `state.tasks` 的锁内检查取消标志并置
`committing`；`cancel_task` 在同一把锁内看到 `committing` 就拒绝。

- **取消获准 ⇒ 目标一定没被写。** 排队、连接、读取阶段都套着 `cancellable`，取消标志一置位就停下，
  报 `cancelled`，消息写明「目标尚未写入」。取消请求本身不把任务直接标成已取消，要等传输真的停下才报。
- **进入 `restoring` 后拒绝取消**，等待实际回包。
- 提交之后的通信中断或超时单独标为 **`uncertain`**（`outcome_unknown`），不能误报 `cancelled`，
  也不能说「未写入」——请求可能已经执行。**不自动重试**；界面提供严格的 `EXISTS` 核对。
- 提交之前的阶段超时是 `failed`（`read_timeout`），这时可以确定目标未写入。

## 大值边界

- 单 Key 序列化上限以 `MAX_DUMP_BYTES` 为准（64 MiB）。`MEMORY USAGE` 可用时在快照脚本里做保守预检
  （`pcall`，被 ACL 挡掉就跳过），`DUMP` 后再核对实际长度。
- `DUMP` 没有范围读取，所以这个上限**不是**服务端序列化峰值内存的硬上限：超限的值已经在服务端序列化过
  一次才被拒绝；客户端打包 `RESTORE` 也可能额外复制缓冲。
- Stream 的消费组 / PEL、STRING 里的 HLL 按原生快照复制。不承诺恢复后的内部编码、内存占用、
  LRU / LFU 统计相同。

## 不做

- **覆盖式写入（`REPLACE`）。** 新建与克隆都不覆盖已有 Key，同名一律停止；并发同名由服务端原子拒绝。
- **把 Lua 当回滚，或写失败后 `DEL` 清理。** 见「创建原子性」。
- **逐成员写初始内容。** 一种类型一条命令，见「创建原子性」。
- **经查看器或 codec 读出值再重建。** 截断、有损、丢内部状态（PEL、模块数据），所以统一走原生快照。
- **按客户端墙钟换算 TTL，或从 `RESTORE` 时重新计时。** 见「跨服务器 TTL」。
- **提交后取消、结果未知时自动重试。** 见「任务状态与取消」。
- **克隆期间锁住源。** 快照之后的变动不追溯，这是单 Key 原子快照的语义。
- **批量克隆与超过上限的 Key**，见 [backlog](../backlog.md)「克隆只能一个一个来，大 Key 克隆不了」。
