# Tsubasa Redis Manager

Vue 3 + Tauri v2 桌面 Redis 客户端。ARDM 的演进版本，但**代码完全独立重写**。

文档在 `doc/`，从 [doc/README.md](doc/README.md) 进入。本文件只放每个任务都要知道的硬规则，
理由与细节在链接的文档里。

## 硬约束：来源与许可

这是这个仓库存在的前提，比任何技术选择都优先。

- **不要把第三方源码复制进仓库。** `PROVENANCE.md` 规定任何复制进来的第三方源码
  都必须在 `THIRD_PARTY_NOTICES.md` 里登记版权与许可声明。因此 shadcn-vue
  **刻意没有采用**（它的分发方式就是复制组件源码进你的仓库），改用它底层的
  `reka-ui` 作为普通 npm 依赖 —— 行为用 reka-ui，外观用本仓库自己的 CSS。
- **不要参考或移植 ARDM 的前端代码。** 功能行为、Redis 协议行为、Tauri 命令契约
  可以作为兼容性要求；组件结构、样式、文案、资源、测试必须独立编写。
- 新增运行时依赖后跑 `node scripts/dev/gen-notices.mjs`，重新生成
  `THIRD_PARTY_NOTICES.md`（清单）与 `THIRD_PARTY_LICENSES.txt`（许可证全文）。
  **这两个文件是生成的，不要手改** —— 只有 `MANUAL_NOTES` 那一节是手写的。
- 本仓库以 MIT 发布（`LICENSE`）。monaco 打包的 Codicon 字体是 CC BY 4.0，
  归属显示在设置→关于里 —— 这类许可要求署名传达给**运行程序的人**，只写进
  仓库文件不算。同页的「查看全文」打开随程序分发的 `THIRD_PARTY_LICENSES.txt`
  （`bundle.resources` 声明，`native_open_licenses` 解析路径）。

## 常用命令

```powershell
npm run dev          # vite dev server（127.0.0.1:1420）
npm run tauri:dev    # 完整桌面应用
npm run check        # build + clippy + cargo test（不需要 Redis）
npm test             # 前端 vitest
./scripts/dev/e2e.ps1        # 起本地 Redis（单机 + 集群）跑 e2e；-Down 拆掉
```

- **e2e 测试默认 `#[ignore]`，不要去掉** —— `npm run check` 必须在没有 Redis 的机器上也能跑。
  改动 `services/values.rs` 或连接层之后跑一次 e2e。见 [development](doc/development.md)。
- `vue-tsc` 开着 `noUnusedLocals` / `noUnusedParameters` / `erasableSyntaxOnly`，
  所以**不能用 enum、不能用参数属性**，改用 `as const` 对象 + 联合类型。

## 文档

- **文档描述系统现在的样子，与代码同一个提交修改。** 改到某段代码时，按
  [doc/README.md](doc/README.md) 的「代码路径 → 文档」对照表核对对应文档。
- 实现过程、返工、踩坑**不进文档**：写进提交信息，或写在出事的那行代码旁。
- 未实现的设计写 `doc/proposals/`，未排期的需求写 `doc/backlog.md`。
- 引用文档写「路径 + 标题原文」，如 `doc/backend/tasks.md「进度口径」`；标题不编号。

## 前后端分层

- **前端不发裸 Redis 命令。** 唯一例外是 CLI 标签页（`redis_execute_cli`）。功能路径走
  `services/values.rs` 的语义化命令或 `services/task.rs` 的任务。`src/api/` 是 Tauri 调用的
  唯一入口，里面**刻意没有**通用的「发一条命令」函数 —— 见 [ipc-contract](doc/backend/ipc-contract.md)「不做」。
- `src/types/ipc.ts` 是 Rust `models.rs` 与 `values.rs` 的手工镜像，**改后端就要改这里**，没有生成器。
- **参数形状按每种类型真实的分页语义设计**：Hash/Set 游标、List/ZSet 下标区间、Stream ID 区间，不做统一抽象。
- **值一律用 `RedisText`**（`text` 与 `base64` 恰好一个非空）；写入参数一律是 base64 字节。
- **截断由后端做并带回 `totalBytes`；`totalBytes` 非空时编辑器必须只读** —— 保存截断值会静默丢数据。
  见 [ipc-contract](doc/backend/ipc-contract.md)「截断契约」。
- **只读属于会话，不属于连接配置。** 前端只认 `ConnectionOpened.readonly`；改完配置要作废该连接
  已开的会话。写命令在 `connection::writable_runtime` 判会话状态，`services/readonly.rs` 的命令名匹配
  **只服务 CLI**。会话是 **DB 级**的，切 DB 等于新开会话。见 [sessions](doc/backend/sessions.md)。
- **集群下 pipeline 必须说明落点**（`executor::PipelineTarget`）：批量写要么按节点分组、要么逐 key。
  打错节点不会出错（redis-rs 跟随 MOVED），但每条命令多一跳，批量的收益全没了；所以验证路由要看服务端的
  `rejected_calls`，不能看结果。分好组也**不能合成一条多 key 命令**（要求同槽位，不只是同节点，CROSSSLOT
  是真报错）。单机测不出来。见 [cluster](doc/backend/cluster.md)。
- **从前缀拼 MATCH 模式必须转义 glob 元字符**（`escapeGlob`），用户手输的模式不转义；
  **批量任务的模式原样发出**。见 [key-tree](doc/frontend/key-tree.md)「前缀生成模式必须转义 glob 元字符」
  与 [tasks](doc/backend/tasks.md)「模式原样」。
- **pipeline 开了 `ignore_errors`**：单条失败是 `Value::ServerError` 那一格，用 `optional_value` 读每一格。
- **连接超时是显式设的**（`connection::REDIS_RESPONSE_TIMEOUT`）；redis-rs 单机默认 500ms、集群默认无超时，
  两边都不能用。Monitor / 订阅走 `open_dedicated_client`，不设响应超时。见 [sessions](doc/backend/sessions.md)「超时」。
- **依赖服务端版本 / 参数 / 模块的功能走能力注册表**（`services/capabilities.rs`），**不按 `redis_version` 判**
  （Valkey 8+ 固定报 7.2.4）。功能连同自己的登记项一起提交；前端只认 `Capability.usable`，提示原样用后端的
  `reason`；后端入口 `require`、受保护命令 `guard`。权限不是能力：`NOPERM` 走 `AppError::PermissionDenied`。
  见 [capabilities](doc/backend/capabilities.md)。
- **STRING 保存绝不退回裸 `SET`**（会清 TTL）：`KEEPTTL` 不可用走脚本，脚本也不可用就拒绝。
- **List 按下标删除是 Lua 原子操作**（`DELETE_LIST_ITEM_SCRIPT`），不要拆成 `LSET` + `LREM` 两条。
- **`pickle` 与 `javaSerialize` 不能做成 RoundTrip** —— 禁的是 encode 与实例化，不是解析。
  见 [codec](doc/backend/codec.md)「Readonly 安全边界」。

## 界面规则

三条硬规则，任何新界面都要遵守，展开见 [visual-language](doc/frontend/visual-language.md)：

1. **Redis 红 `#d6372b` 只用于品牌与危险** —— 品牌标记、只读/生产标识、删除类操作、错误信息。
   任何「强调」语义一律用蓝色 `--accent`。
2. **类型色只出现在三处** —— 树/列表的圆点、标签页的圆点、Key 元信息头的徽标。表格内部不上色。
3. **等宽字体只给数据。** 判据是「这段文字是不是 key 路径的一部分」；连接名是用户起的标签，用界面字体。

其余约定：

- 颜色、尺寸、字号一律走 `styles/tokens.css` 的变量，不要写字面值
- 图标只用 12 / 16 两档，在 `lib/icons.ts` 集中登记后再用
- 树与数据表的行高在 CSS 变量和虚拟化的 JS 常量里各有一份，**改一处要改两处**
- 类名不要起 `.empty` 这类泛化名字，会和别处的同名类撞车
- 会渲染数字的地方配 `font-variant-numeric: tabular-nums`
- 改界面前先读对应的 `doc/frontend/` 专题

## 性能敏感区

Key 树、数据表、Monaco、任务事件流。前两者必须虚拟化，详见
[architecture](doc/architecture.md)「性能敏感区」。

- 装递归类型的 ref（codec 的 `Node` / `CodecChain`、CLI 的 `RedisValue`）**一律 `shallowRef`** ——
  `ref` 会让 `UnwrapRef` 把 TS 类型实例化撑爆（TS2589）
- Monaco 走动态 `import()` 按需装配，不要 `import 'monaco-editor'` 整包
- `redis://task-progress` 是唯一高频频道：条目进 `shallowRef` 数组、逐帧 flush、设环形上限并显示丢弃条数

## Tauri 注意事项

- `src-tauri/capabilities/default.json` **必须存在**。没有它 webview 拿到空 ACL，
  连 `listen()` 都会被拒。`core:window:allow-start-dragging` 与
  `allow-start-resize-dragging` 不在 `core:default` 里，缺了自绘标题栏拖不动窗口，
  而且不报错、只是没反应。
- `decorations: false`，标题栏与八个缩放热区都是自绘的。
- `devUrl` 用 `127.0.0.1` 而不是 `localhost`：Windows 上后者常先解析到 `::1`，
  而 vite 只监听 IPv4。

## 环境

Windows 开发。本机 HTTP 代理会拦截对 `127.0.0.1` 的请求并回 503 ——
用 curl 探测本地服务时记得加 `--noproxy '*'`，否则会把代理的错误当成服务的错误。
