# 架构

覆盖：`src/`、`src-tauri/src/` 的目录划分与跨层边界；性能敏感区：`src/components/sidebar/KeyTree.vue`、`src/components/sidebar/sidebarRows.ts`、`src/components/ui/VirtualTable.vue`、`src/lib/monaco.ts`、`src/lib/monacoLoader.ts`、`src/components/stream/useTaskStream.ts`

Vue 3 + Tauri v2 的桌面 Redis 客户端。前端只通过 Tauri 命令与 Rust 后端交互；
Redis 连接、协议细节与长耗时任务都在后端。

## 目录职责

| 路径 | 职责 |
|---|---|
| `src/api/` | Tauri 调用的唯一入口，别处不直接 import `invoke`。没有通用的「发一条命令」函数 |
| `src/types/ipc.ts` | Rust `models.rs` 与 `services/values.rs` 的 TS 镜像，手工同步，没有生成器 |
| `src/lib/` | key 路径与紧凑合并、格式化、TTL、编解码器、CLI 行编辑、Monaco 装配 |
| `src/stores/` | settings / connections / sessions / tabs / tasks / clones / ui |
| `src/components/` | ui（reka-ui 封装）、shell、sidebar、keys、browser、cli、stream、dialogs |
| `src/styles/` | 令牌与组件样式 |
| `src-tauri/src/commands/` | 暴露给前端的 Tauri 命令 |
| `src-tauri/src/services/values.rs` | 按类型划分的语义化取值 / 写入命令，功能路径的主入口 |
| `src-tauri/src/services/executor.rs` | 连接抽象与 pipeline，`PipelineTarget` 管集群落点 |
| `src-tauri/src/services/capabilities.rs` | 服务端身份与能力注册表，建连时探测、运行时降级 |
| `src-tauri/src/services/task.rs` | 长耗时任务：扫描、批量删除 / TTL、导入导出、Monitor、订阅、内存分析 |
| `src-tauri/src/services/key_write.rs`、`clone_task.rs` | 新建 Key 与跨连接克隆 |
| `doc/fe_ref/` | 静态 HTML 视觉原型，浏览器直接打开；只表达视觉，行为以本目录文档为准 |

## 前后端分层

四条原则，命令层的具体形状见 [backend/ipc-contract](backend/ipc-contract.md)：

1. **前端不发裸 Redis 命令。** 唯一例外是 CLI 标签页——那里用户手输的就是命令，
   `redis_execute_cli` 是它的正当入口。功能路径一律走按类型划分的语义化命令
   （`services/values.rs`）或任务（`services/task.rs`）。
2. **协议知识住在 Rust。** RESP2 / RESP3 的形状差异、三种分页语义、集群路由、
   pipeline 批处理、Redis 内部编码（如 HLL 的寄存器布局），前端一律不感知。
   前端拿到的是已经吸收掉这些差异的结构。
3. **只读性由命令定义保证，不由字符串匹配保证。** 取值命令天生只读，写命令在入口
   判会话状态；命令名匹配只服务 CLI。见 [backend/sessions](backend/sessions.md)「只读属于会话」。
4. **一次交互一次 IPC。** 打开一个 key 取元信息是一次 `redis_get_key_meta`，
   后端在里面 pipeline 多条命令；SCAN 回来的 key 自带类型，不需要前端再逐个补 `TYPE`。
   批量、并发调度这类事不该出现在渲染进程。

现存的例外：Key 浏览器的分析模式（`useKeyAnalysis`）仍是逐 key 调
`redis_get_key_meta`，由前端做 8 路并发。

`src/api/` 是 Tauri 调用的唯一入口，里面刻意没有通用的「发一条命令」函数——
理由见 [backend/ipc-contract](backend/ipc-contract.md)「不做」。

## 性能敏感区

四处：Key 树（`.tree`）、数据表（`.dt`）、Monaco、任务事件流。
`doc/fe_ref/` 原型里的 DOM 结构只是视觉目标，前两处必须虚拟化（`@tanstack/vue-virtual`）。

### Key 树

- 侧栏是一棵复合树：连接 → DB → namespace → key。`useSidebarRows`（`sidebarRows.ts`）
  先把三种来源拼成**一维行序列**，再交给 `KeyTree.vue` 虚拟化；渲染层只画行，
  不关心行从哪来。
- **紧凑路径在数据层算完**（`lib/keyTree.ts` 的 `flattenTree`），渲染层只拿 `TreeRow`。
- 建树结果放 `shallowRef`（`sessions.ts` 的 `treeCache`）+ 显式 `triggerRef`，
  不让 Vue 深层代理几万个节点。
- 展开 / 折叠状态存在 store 的 Map / Set 里（`expandedPaths` 等），不依赖 DOM——
  虚拟化之后不在视口里的行根本没有 DOM。
- 浏览路径只在用户点「加载更多」时续扫。已加载 key 数触到「树节点上限」（应用设置
  `treeNodeLimit`，不是连接配置）后，底部行改成「已达树节点上限，继续加载」，
  把「已经到上限」说出来，仍由用户显式继续；搜索与前缀扫描触到上限则取消任务。

### 数据表

`components/ui/VirtualTable.vue`。`<table>` 的行没法绝对定位，所以 `.dt.is-virtual`
把 table 换成 grid / flex，**列宽由组件统一下发到 th 与 td**——消费方只给单元格内容、
不碰宽度，否则表头和行迟早对不齐。

### 行高有两份

树与数据表的行高在 `tokens.css`（`--h-tree`、`--h-row`，随密度切换）和虚拟化的 JS 常量
（`KeyTree.vue`、`VirtualTable.vue` 里的 `ROW_HEIGHT`）里各有一份。虚拟化必须在 JS 里
知道行高，**改一处要改两处**。

### Monaco

- 近 4 MB，走动态 `import()`，不进首屏。入口 `lib/monacoLoader.ts` 单独成文件：
  它若和 `lib/monaco.ts` 放在一起，调用它就已经要求整包加载完，动态 import 白做。
- 按需装配（`lib/monaco.ts`：`editor.api` + 明确列出的 contrib + JSON 语言）。
  不 `import 'monaco-editor'` 整包——那会把 80 多种语言定义全拉进来。
- 应用启动后在空闲时预热（`App.vue` 的 `prewarmMonaco`），打开 STRING 编辑器时
  复用同一个 promise。它是应用级资源，所以预热不挂在任何会话或扫描结果上。

### 任务事件流

`redis://task-progress` 是唯一的高频频道，Monitor 一秒可能上千条。
消费方统一走 `components/stream/useTaskStream.ts`：

- 条目进 `shallowRef` 的普通数组，**逐帧 flush**——每条都触发响应式更新，滚动会直接卡死。
- **环形上限**，超出丢最旧的；上限由各消费方传入。挂一夜的 Monitor 否则会把内存吃光。
- 丢掉的条数记在 `dropped` 里，界面要说出来，不能装作没发生。Monitor 标签页照此显示；
  订阅标签页目前没有显示。

### 递归类型与大集合用 `shallowRef`

装递归类型的 ref（codec 的 `Node` / `CodecChain`、CLI 的 `RedisValue`）一律 `shallowRef`：
`ref` 会让 Vue 的 `UnwrapRef` 把 TypeScript 的类型实例化撑爆（TS2589）。
集合成员列表同理用 `shallowRef`，给几万个成员逐个做响应式代理毫无必要。
