# Tsubasa Redis Manager 文档

协作硬约束见 [CLAUDE.md](../CLAUDE.md)。本目录描述系统**现在**是什么样、为什么只能是这样；
读文档与读代码应当得到同一个答案。

## 从任务进入

| 要做什么 | 先读 |
|---|---|
| 找代码入口、理解进程边界与前后端分层 | [architecture](architecture.md) |
| 运行、测试、e2e、CI 与发布、许可清单 | [development](development.md) |
| 改界面 | [frontend/visual-language](frontend/visual-language.md)，再读对应的 `frontend/` 专题 |
| 加或改 Tauri 命令 | [backend/ipc-contract](backend/ipc-contract.md) |
| 改连接、会话、集群路由 | [backend/sessions](backend/sessions.md)、[backend/cluster](backend/cluster.md) |
| 功能依赖服务端版本、参数或模块；处理权限错误 | [backend/capabilities](backend/capabilities.md) |
| 改长耗时任务（扫描、批量、导入导出、克隆） | [backend/tasks](backend/tasks.md)；克隆另见 [backend/key-write](backend/key-write.md) |
| 加或改编解码器 | [backend/codec](backend/codec.md) → [frontend/codecs](frontend/codecs.md) → [frontend/codec-formats](frontend/codec-formats.md) |
| 看下一步做什么 | [roadmap](roadmap.md) → [proposals/](proposals/)；未排期的在 [backlog](backlog.md) |

## 代码路径 → 文档

改到左列的路径，就在同一个提交里核对右列的文档。

| 代码路径 | 文档 |
|---|---|
| `src/`、`src-tauri/src/` 的目录划分与跨层边界 | [architecture](architecture.md) |
| `package.json` 脚本、`scripts/dev/`、`.github/workflows/`、`rust-toolchain.toml`、三处版本号（`package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json`） | [development](development.md) |
| `src/components/sidebar/KeyTree.vue`、`src/components/sidebar/sidebarRows.ts`、`src/components/ui/VirtualTable.vue`、`src/lib/monaco.ts`、`src/lib/monacoLoader.ts`、`src/components/stream/useTaskStream.ts` | [architecture](architecture.md)「性能敏感区」 |
| `src-tauri/src/commands/`、`src-tauri/src/services/values.rs`、`src-tauri/src/services/redis_values.rs`、`src-tauri/src/models.rs` | [backend/ipc-contract](backend/ipc-contract.md) |
| `src/api/`、`src/types/ipc.ts`、`src/lib/redisText.ts` | [backend/ipc-contract](backend/ipc-contract.md) |
| `src-tauri/src/services/connection.rs`、`src-tauri/src/services/readonly.rs`、`src-tauri/src/state.rs`、`src-tauri/src/services/cluster.rs`（超时） | [backend/sessions](backend/sessions.md) |
| `src/stores/sessions.ts`、`src/components/dialogs/ConnectionDialog.vue` | [backend/sessions](backend/sessions.md) |
| `src-tauri/src/services/capabilities.rs`、`src-tauri/src/services/connection.rs`（`probe_server`）、`src-tauri/src/state.rs`（`capabilities`、`capability_sink`）、`src-tauri/src/error.rs`（`PermissionDenied`、`CapabilityUnsupported`）、`src-tauri/src/services/values.rs`（`set_string_value`）、`src/lib/capabilities.ts`、`src/stores/sessions.ts`（`applyCapabilityChange`）、`src/api/events.ts`、`src-tauri/tests/redis_capabilities.rs` | [backend/capabilities](backend/capabilities.md) |
| `src/stores/tabs.ts`、`src/components/shell/TabBar.vue`、`src/components/shell/useShortcuts.ts`、`src/components/main/MainArea.vue`（`renderKey`）、`src/components/shell/StatusBar.vue`（内存读数入口）、`src/components/cli/`、`src/lib/lineEdit.ts` | [frontend/tabs](frontend/tabs.md) |
| `src/styles/tokens.css`、`src/styles/base.css`、`src/lib/icons.ts`、`src/components/ui/Icon.vue`、`src/components/ui/TypeDot.vue`、`src/components/ui/TypeChip.vue`、`src/stores/settings.ts`（`applyToDocument` 的字体替换） | [frontend/visual-language](frontend/visual-language.md) |
| `src/components/keys/TtlChip.vue`、`src/components/keys/TtlModePicker.vue`、`src/lib/ttl.ts`（`useTtlCountdown`、`useTtlDraft`、`ttlCommandFor`）、`src/lib/format.ts`（`formatTtl`、`ttlState`）、`src/components/keys/KeyTab.vue`（TTL chip、重同步、失效态）、`src/components/keys/useKeyMeta.ts` | [frontend/ttl](frontend/ttl.md) |
| `src-tauri/src/services/codec.rs`、`src-tauri/src/commands/native.rs`（`native_codec_*`） | [backend/codec](backend/codec.md) |
| `src/lib/codec/index.ts`、`src/lib/codec/types.ts`、`src/lib/codec/chain.ts`、`src/lib/codec/render.ts`、`src/lib/codecs.ts` | [frontend/codecs](frontend/codecs.md) |
| `src/components/keys/viewers/CodecPicker.vue`、`src/components/keys/viewers/ValueDrawer.vue`、`src/components/keys/viewers/StringViewer.vue`（解码链与保存） | [frontend/codecs](frontend/codecs.md) |
| `src/lib/codec/php.ts`、`src/lib/codec/msgpack.ts`、`src/lib/codec/protobuf.ts`、`src/lib/codec/pickle.ts`、`src/lib/codec/java.ts` | [frontend/codec-formats](frontend/codec-formats.md) |
| `src/components/dialogs/BatchTtlDialog.vue`、`src/components/dialogs/BatchDeleteDialog.vue`、`src/stores/ui.ts`（批量对话框目标）、`src/lib/ttl.ts`（条件 flag）、`src/styles/overlay.css`（`.bulk-*`） | [frontend/batch](frontend/batch.md) |
| `src/components/sidebar/KeyTree.vue`（批量入口）、`src/stores/sessions.ts`（`forgetPrefix`、`invalidateKeyspace`） | [frontend/batch](frontend/batch.md) |
| `src/components/browser/KeyBrowserTab.vue`、`src/components/browser/useKeyAnalysis.ts`、`src/components/ui/VirtualTable.vue`（复选框列）、`src/stores/settings.ts`（`analysisMode`） | [frontend/key-browser](frontend/key-browser.md) |
| `src/lib/keyTree.ts`、`src/styles/tree.css`、`src/lib/format.ts`（`formatPartialCount`、`formatApproxPercent`）、`src/stores/settings.ts`（`compactPaths`、`indentGuides`、`treeTtl`、`defaultTreeMode`） | [frontend/key-tree](frontend/key-tree.md) |
| `src/stores/sessions.ts`（展开态、搜索、前缀扫描与前缀权威）、`src/components/sidebar/KeyTree.vue`（行网格、分段命中、计数徽标、TTL 标记、搜索态行、「在此搜索」「扫描此前缀」）、`src/components/sidebar/sidebarRows.ts`、`src/components/sidebar/Sidebar.vue`（模式切换与紧凑开关、漏斗弹层、搜索框、作用域 chip、窗口隐藏暂停） | [frontend/key-tree](frontend/key-tree.md) |
| `src-tauri/src/services/task.rs`（批量删除、批量 TTL、试算；`run_scan_task`、节流、`ScanProgressEstimator`、`normalize_scan_count`、`normalize_scan_pattern`）、`src-tauri/src/models.rs`（批量任务参数、`TaskEventItem::Key`、`estimated_progress`）、`src/components/shell/StatusBar.vue`（`TASK_TYPE_LABEL`、近似进度） | [backend/tasks](backend/tasks.md) |
| `src/components/dialogs/KeyWriteDialog.vue`、`src/components/dialogs/KeyBytesInput.vue`、`src/lib/keyCreation.ts`、`src/stores/clones.ts`、`src/stores/ui.ts`（`openCreateKey`、`openCloneKey`）、`src/styles/keywrite.css`、`src/components/shell/StatusBar.vue`（「克隆任务」菜单） | [frontend/key-write-dialog](frontend/key-write-dialog.md) |
| `src-tauri/src/services/key_write.rs`、`src-tauri/src/services/clone_task.rs`、`src-tauri/src/services/connection.rs`（`open_transfer_runtime`）、`src-tauri/src/services/task.rs`（`cancel_task` 的克隆分支）、`src-tauri/src/models.rs`（`ClonePhase`、`CloneTaskSnapshot`、`TaskEventItem::Clone`） | [backend/key-write](backend/key-write.md) |
| `src/components/keys/viewers/HllViewer.vue`、`src/components/keys/KeyTab.vue`（HLL 分发、中性 tag、`HLL` 元信息格）、`src/styles/hll.css`、`src/styles/tokens.css`（`--hx-*`） | [frontend/hll-viewer](frontend/hll-viewer.md) |
| `src-tauri/src/services/hll.rs`、`src-tauri/src/services/values.rs`（`get_hll_count`、`add_hll_elements`、`inspect_hll`、`HLL_INSPECT_FETCH_LIMIT`）、`src-tauri/src/services/connection.rs`（`get_key_meta` 的 HLL 探针）、`src-tauri/src/services/readonly.rs`（`PFCOUNT`）、`src-tauri/src/models.rs`（`HllMeta`） | [backend/hll](backend/hll.md) |
| `src-tauri/src/services/executor.rs`、`src-tauri/src/services/task.rs`（`KeyBatch`、`ScanPage`、`scan_cluster_one_page`、`delete_key_chunk`、`apply_ttl_batch`、分块常量）、`src-tauri/src/services/connection.rs`（`annotate_keys`、集群游标编解码、`scan_cluster_keys`） | [backend/cluster](backend/cluster.md) |

## 文档规范

### 按时态分三类

| 时态 | 内容 | 位置 | 生命周期 |
|---|---|---|---|
| 现在时 | 系统现在是什么样，以及为什么只能是这样 | `frontend/`、`backend/`、`architecture.md`、`development.md` | 与代码同一个提交修改，始终有效 |
| 将来时 | 未实现的设计、现状分析、排期 | `proposals/`、`roadmap.md`、`backlog.md` | 实现后并入现在时文档，提案删除 |
| 过去时 | 实现过程、返工、踩坑 | 提交信息；踩坑写在出事的那行代码旁 | 不进 `doc/` |

### 现在时文档的准入标准

- **每一句都能对照代码核验。** 「v0.1 有 X，结果 Y」改写成「没有 X：有了它就会 Y」——
  教训留下，故事去掉。
- **只写代码说不出的东西**：跨模块的约束、刻意不存在的东西、否决过的方案及理由。
  只涉及一个函数的理由写代码注释。
- **不复述会变的事实。** 命令清单、常量值、类型定义写「以 `xxx` 为准」，不抄一份。
  像素值、字号以 `src/styles/tokens.css` 为准，文档只写用途与变量名。
- **用符号名锚定代码，不用行号。**
- **不写版本号。** 「v0.3 已做」「片 4 落地时」属于过去时。
- **「不做」是正文的一部分。** 文档末尾可以有「不做」一节，写否决的方案与理由——
  它防的是刻意的缺席被当成疏漏补上。

### 引用约定

- 代码注释与文档互链写 `doc/backend/tasks.md「进度口径」`：文件路径 + 标题原文。
- **标题不编号。** 编号记的是写作顺序，插一节就全体漂移；标题文字可以 grep，改标题时一起改引用。
- 每份现在时文档开头一行写 `覆盖：` 列出它管的代码路径，并汇总进上面的对照表。

### 提案

`proposals/` 里的文档可以写现状分析与排期，允许随时间过时，因为它们活不长：
实现之后，设计部分改写成现在时并入 `frontend/` / `backend/`，提案本身删除。
放弃的提案同样删除，否决理由写进对应现在时文档的「不做」。
