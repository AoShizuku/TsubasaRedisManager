# 会话

覆盖：`src-tauri/src/services/connection.rs`、`src-tauri/src/services/readonly.rs`、`src-tauri/src/state.rs`、`src-tauri/src/services/cluster.rs`（超时）、`src/stores/sessions.ts`、`src/components/dialogs/ConnectionDialog.vue`

会话是后端在 `redis_open_connection` / `redis_open_stored_connection` 时建立的运行态
（`state::RedisSession`）。**db、只读、凭据、SSH 隧道都在打开那一刻定死**，会话上存的是
当时那份配置的快照；之后改连接配置不会追溯到已开的会话。

## 会话是 DB 级的

- 一个会话对应 `(连接, db)`。展开 db3 会为它单独开一个会话；前端按
  `sessionKeyOf(connectionKey, db)` 去重（`stores/sessions.ts` 的 `openDb`）。
- 展开一个连接时按它配置的默认 db 打开，而不是无脑 db0。
- 集群只有 db0。
- 这决定了前端 store 的形状：key 列表、扫描游标、树、展开路径都按 sessionId 存。
  切 DB 不是在会话上切，是换一个会话。

## 只读属于会话

只读判定读的是会话上的 `readonly`，按流量分三处守：

- **功能路径的写命令**：`connection::writable_runtime` 在入口显式判会话状态（`services/values.rs`、
  `key_write.rs` 的每条写命令都从它拿 runtime）。读写性质在编译期就确定了，取值命令天生只读，
  不需要运行时猜。
- **写类任务**：导入（`task::ensure_session_writable`）、批量删除与批量 TTL、克隆的目标端，
  各自在启动时判。批量任务的**试算放行**（`start_batch_delete_task` / `start_batch_ttl_task` 里的
  `readonly && !dry_run`）：试算不写任何东西，拒绝的是写，不是这个任务——「先看看有多少」正是
  只读连接上会做的事。见 [tasks](tasks.md)「试算」。
- **CLI**：`services/readonly.rs` 按命令名匹配，「不在允许列表里就算写」。这种猜法只在 CLI
  站得住——输入是自由文本，除了命令名没有别的判据。**它只服务 CLI**，不要拿它管功能路径。

前端两条硬约束：

- **`ConnectionOpened.readonly` 是前端唯一的只读依据。** 它进 `Session.readonly`，编辑控件
  一律照它禁用。不要从 `ConnectionSummary` 或编辑草稿再推断一遍——那是两套判断，会漂移，
  危险的方向是「界面能编辑、守卫也放行」：配置改成只读之后，之前开的会话仍然可写。
  连接行上的「只读」标签展示的是配置，不参与放行。
- **改完连接配置必须作废它已开的会话**（`ConnectionDialog.vue` 的 `save`：先关标签页，
  再 `sessions.closeConnection`）。侧栏折叠连接并不断开会话，所以「看起来没连着」也可能有
  活会话被复用。删除连接同样先断开再删。

覆盖这两条的 e2e：`tests/redis_values.rs` 的 `readonly_sessions_report_themselves_and_reject_writes`
（功能路径），`tests/redis_basic.rs` 的 `readonly_connection_rejects_write_commands`（CLI）。

## 能力属于会话

服务端身份与能力在建连时探测一次（`ConnectionOpened.server` / `capabilities`），存在会话的 runtime 上，
会话内只会被降级、不会变好，不跨会话缓存——要重新探测就重开会话，改完配置作废会话时自然发生。
前端只认会话上的那份。规则见 [capabilities](capabilities.md)。

## 超时

单机与集群的响应超时、建连超时都是**显式设的**，以 `connection::REDIS_RESPONSE_TIMEOUT`、
`REDIS_CONNECTION_TIMEOUT` 为准。redis-rs 的默认值两头都不合适：

- 单机默认响应超时 500ms。跨 WAN 时 RTT 就吃掉一大截预算，取值命令最多要搬回一个截断窗口，
  在有实际延迟的链路上不可能达标；症状是随机的 timed out，越大的 key 越必现。
- 集群默认响应超时是 `None`，一个卡住的节点会让界面永远等下去。

建连超时比响应超时短：连不上要快速失败，连上之后要有耐心。建连重试次数
（`REDIS_CONNECTION_RETRIES`）同样压低了，理由在常量旁。

## 专用连接

不是所有流量都走会话的 executor。会话的 executor 在一把锁后面，同一会话的命令串行：

- **Monitor / 订阅**走 `connection::open_dedicated_client` 的裸 client。它们是长连接，
  不能有响应超时，也不能占住会话。配置取自会话快照（`session_config`），所以与会话同 db、
  同凭据。集群会话不支持。
- **克隆传输**走 `connection::open_transfer_runtime`：独立的连接、SSH 隧道与更长的响应预算，
  一次大搬运不会把浏览会话的界面操作挡住。

## 不做

- **在现有会话上 `SELECT` 切库。** 功能路径里没有 `SELECT`。一个会话被多个标签页与任务共享，
  在它上面切库会让它们全部悄悄换到别的库；只读会话的 CLI 也会拦下 `SELECT`（不在允许列表里）。
  可写会话的 CLI 目前**不拦** `SELECT`，见 [backlog](../backlog.md)「CLI 里的 SELECT 会让整个会话换库」。
- **给 `readonly.rs` 的允许列表加 `MEMORY` / `CONFIG`。** 功能路径本来就不经过它，没有需要；
  而这两个命令的子命令读写混合，开了洞，CLI 里手输的 `MEMORY PURGE`、`CONFIG SET` 跟着放行，
  除非再补一层子命令判断——为绕行方案再加一个绕行方案。
- **从连接配置推断只读**，见「只读属于会话」。
