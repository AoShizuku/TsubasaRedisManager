# Tsubasa Redis Manager — v0.1 前端设计原型

静态 HTML 原型，只覆盖**浅色模式**。用浏览器直接打开 `index.html` 即可，无需构建、无外部网络依赖。

```powershell
Start-Process "doc\fe_ref\index.html"
```

## 文件

| 文件 | 内容 |
|---|---|
| `index.html` | 原型索引 + 色板 + 组件预览 + 尺寸规格 |
| `01-main-idle.html` | 主窗口，未选中任何实例 |
| `02-main-connected.html` | 主窗口，展开一个实例（树 + 标签 + HASH 编辑器 + 右键菜单） |
| `03-connection-new.html` | 新建连接对话框（5 个分页可点击） |
| `04-settings.html` | 设置对话框（8 个分组可点击） |
| `05-key-viewers.html` | 六种 Key 类型 + CLI 的显示样式（点标签切换）；ZSET 标签内含 TTL 编辑浮层 |
| `06-tree-modes.html` | 紧凑路径（单链合并）、分段命中、缩进参考线，以及主区的 Key 浏览器分析表 |
| `07-hyperloglog.html` | HyperLogLog 查看器（STRING 子形态）：基数仪表盘 + 结构查看器（头部染色解析 + 寄存器热力图 / 原始 hex），损坏态为同一查看器的降级（点标签切换） |
| `assets/tokens.css` | 设计令牌，可直接搬进 Vue 工程的 `:root` |
| `assets/ui.css` | 原型组件样式 |
| `assets/proto.js` | 图标 sprite、标签切换、缩放控制（仅原型用） |

每个原型页右上角有 100% / 80% / 65% 缩放按钮，首次打开会根据窗口宽度自动选一档。

## 设计取向

骨架继承 ARDM，皮肤完全重画：

- **保留**：`连接 → DB → namespace → key` 的单树导航、按分隔符折叠 namespace、按需加载、左树右编辑器、Key 与 CLI 都能作为标签打开、每种类型有原生编辑器、Viewer 管线、全局搜索与值内搜索分离、SCAN 进度可见、右键菜单、连接颜色标签与只读模式。
- **重做**：密度（行高 30–34 px）、去卡片化、细分隔线、中性灰底色、Redis 红只用于品牌与危险、界面字体与等宽字体分工、元信息头固定排布、状态栏常驻。

三条硬规则（红色只给品牌与危险、类型色只出现在三处、等宽字体只给数据）见
[frontend/visual-language](../frontend/visual-language.md)「三条硬规则」。

## 与 Rust 后端的对应关系

原型里的每块信息都能落到已有的 Tauri 命令上，没有画后端不支持的功能：

| 界面元素 | 后端 |
|---|---|
| 侧栏连接列表、颜色圆点、cluster/readonly/ssh/tls/sentinel 徽标 | `connection_store_list` → `ConnectionSummary` |
| 新建连接表单字段 | `ConnectionEditDraft` / `ConnectionSaveDraft` / `connection_store_save` |
| 「集群与哨兵互斥」的页内警告 | `build_connection_plan` 的校验规则 |
| 密码字段旁的「系统密钥链」标记 | `services/credential_store.rs` |
| 「从 ARDM 导入」 | `connection_store_migrate_legacy` |
| Key 树的按需加载、加载更多 | `redis_scan_keys` / `redis_start_load_all_keys` |
| 侧栏搜索框 | `redis_start_search_keys` |
| 状态栏 SCAN 进度、任务计数、停止按钮 | `TaskProgressEvent` / `redis_cancel_task` |
| Key 元信息头（类型 / TTL / 值） | `redis_get_key_detail` → `KeyDetail` |
| 值详情条的编码切换与自动识别 | `native_codec_decode` / `encode` / `detect`，九种 `CodecName` |
| 设置 → 数据查看器的编码能力表 | `codec_support_matrix()` 的三种 `CodecConclusion` |
| 设置 → 格式化器（含禁止 `.bat`/`.cmd`、超时、输出上限） | `native_run_formatter` / `FormatterRunOptions` |
| 设置 → Protobuf `.proto` 文件 | `native_read_proto_files` |
| 设置 → 界面缩放 / 界面字体候选 | `native_set_zoom_factor` / `native_get_fonts` |
| 设置 → 检查更新 | `native_check_update` → `UpdateCheckResult` |
| 右键菜单的导出、内存分析、批量删除 | `redis_start_export_keys` / `redis_start_memory_analysis` / `redis_start_batch_delete` |
| CLI 标签、只读拦截 | `redis_execute_cli` / `services/readonly.rs` |
| 侧栏底部的 Monitor、订阅入口 | `redis_start_monitor` / `redis_start_subscription` |
| 标题栏的最小化/最大化/关闭 | `native_minimize_window` / `native_toggle_maximize` / `native_hide_window` |

SSH 分页里「RSA 私钥暂不可用（RUSTSEC-2023-0071）」的提示，对应 README 中已记录的依赖限制。

## 原型 class → 实现

**shadcn-vue 没有采用** —— 它的分发方式是把组件源码复制进你的仓库，那会让第三方
源码进版本管理并需要在本仓库复现它的版权声明。改用它底层的 `reka-ui` 作为普通
npm 依赖：行为用 reka-ui 的无样式原语，外观用 `src/styles/` 自己的 CSS。
见 `THIRD_PARTY_NOTICES.md` 与 `PROVENANCE.md`。

| 原型 class | 实现 |
|---|---|
| `.btn` 及各变体 | `ui/Btn.vue`（原生 `button` + 自有样式） |
| `.input` `.input-wrap` `.select` | 原生元素 + 自有样式 |
| `.sw` / `.cbx` | `ui/Sw.vue` / `ui/Cbx.vue`（reka-ui） |
| `.seg` | `ui/Seg.vue` |
| `.chip` `.tag` | `ui/TypeChip.vue`、`ui/TypeDot.vue` |
| `.dlg` 系列 | `ui/Dlg.vue`、`ConfirmDialog.vue`、`PromptDialog.vue`（reka-ui） |
| `.menu` 系列 | `ui/CtxMenu.vue`、`Menu.vue`、`MenuItem.vue`（reka-ui） |
| 悬浮提示 / 浮层 | `ui/Tip.vue`、`ui/Pop.vue`（reka-ui） |
| `.tabbar` `.tab` `.subtabs` | `shell/TabBar.vue` + `stores/tabs.ts` |
| `.banner` | 自有样式；瞬时消息走 `ui/Toasts.vue` |
| `.dt` 数据表 | `ui/VirtualTable.vue`（自实现 + `@tanstack/vue-virtual`） |
| `.tree` | `sidebar/KeyTree.vue`（紧凑树 + `@tanstack/vue-virtual`） |
| `.editor` | Monaco Editor（按需装配，见 `lib/monaco.ts`） |
| `.tablewrap` 的虚拟滚动 | `@tanstack/vue-virtual` |

`.dt` / `.tree` / `.editor` 三处是性能敏感区，原型里的样式是最终视觉目标，但 DOM
结构必须换成虚拟化实现。原型里的 `.term` 属于尚未实现的范围。

## v0.1 范围之外

原型中已画出但标注为后续版本的部分：

- 深色主题（设置 → 外观中置灰）
- 分割视图（标签栏右侧图标已留位）
- 命令面板 `Ctrl+Shift+P`（omnibar 已留入口）
- 内存分析、Monitor、订阅的独立面板（侧栏底部已留入口，但未画面板本身）
