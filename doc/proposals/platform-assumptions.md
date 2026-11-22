# 去掉代码里的 Windows 假设

> 提案。实现后按「落地去向」并入现在时文档，删除本文。

## 为什么单独一份

开发机是 Windows，CI 与出包也只跑 Windows，所以只在 macOS / Linux 上才会走到的分支从来没被常规编译和运行过。
下面四处在 Windows 上表现正常，换到另外两个平台**大多不报错，而是静默失效**：按键没反应、
拖边没反应、列表加载失败只给一句英文错误。这类问题不会自己暴露，只会变成「Mac / Linux 版不好用」。

webview 引擎本身的差异（WebKitGTK 的 DMABUF 白屏、JSC 的 NaN）不在本文范围。

## 现状

| 位置 | 假设 | macOS | Linux |
|---|---|---|---|
| `useShortcuts.ts`；设置 → 快捷键表、`IdleState.vue`、`TabBar.vue` 菜单项、`TitleBar.vue` 命令面板入口的文案 | 主修饰键是 Ctrl | ⌘B / ⌘W 没反应；文案写着 Ctrl | 正常 |
| `tauri.conf.json`（`decorations: false`）、`TitleBar.vue`、`ResizeHandles.vue` | 窗口按钮在右上角、缩放靠自绘热区 | 没有红绿灯；拖边缩放静默无效 | 正常 |
| `TitleBar.vue` 关闭按钮 → `native_hide_window` | 隐藏后有办法找回窗口 | Dock 图标点了没反应 | 窗口找不回来 |
| `credential_store.rs`、`connection_store.rs` | 系统密钥链总是可用、读取无代价 | 签名变化时可能逐条弹授权 | 没有 Secret Service 时连接列表加载失败 |
| `formatter.rs` | 进程的 `PATH` 就是用户在终端里的 `PATH` | 从 Dock 启动时找不到 Homebrew 装的格式化器 | 视桌面环境而定 |

关闭按钮那一行**在 Windows 上同样成立**：`window.hide()` 连任务栏按钮一起藏掉，没有托盘、没有单实例，
点了关闭的进程只能去任务管理器里结束。放在这里一起改，因为三个平台的修法不同。

## 快捷键：主修饰键

### 应用命令跟随主修饰键，行编辑键不跟

| 操作 | Windows / Linux | macOS | 理由 |
|---|---|---|---|
| 新建 Key 浏览器 | `Ctrl B` | `⌘B` | 应用命令用主修饰键 |
| 关闭当前标签页 | `Ctrl W` | `⌘W` | 同上 |
| 命令面板（占位） | `Ctrl ⇧ P` | `⌘⇧P` | 同上 |
| 新建命令行 | `` Ctrl ` `` | `` Ctrl ` `` | `` ⌘` `` 是系统的「同一应用的窗口间切换」，不能占 |
| 刷新当前视图 | `F5` | `⌘R`，`F5` 保留 | Mac 键盘的 F 键默认是媒体键 |
| CLI 删词 / 清屏 | `Ctrl W` / `Ctrl L` | 不变 | readline 键在 Mac 终端里也是 Ctrl |

**`⌘W` 在输入框里也关标签页。** Windows 上 `Ctrl W` 在输入框里让位，是因为它同时是 readline 的删词键；
Mac 上 `⌘W` 不是文本编辑键，这个理由不成立。CLI 的删词仍然只认 Ctrl，两者不冲突。

### 平台判定同步完成

新增 `src/lib/platform.ts`，导出 `isMac`，用 `navigator.userAgent` 判定。快捷键在启动时同步注册，
不能等一次 IPC 往返。不引入 `@tauri-apps/plugin-os`：为一个布尔值多一个插件、多一条许可登记，不值。

### 快捷键只登记一处

现在快捷键分散在 `useShortcuts.ts` 与四处文案里，`doc/frontend/tabs.md` 要求「改一处要改两处」。
改为一张快捷键表：`useShortcuts.ts` 按它匹配按键，设置页表格、菜单项的 `shortcut`、空闲页、命令面板入口都从它取文案。
Mac 文案用符号且不加空格（`⌘⇧P`），Windows / Linux 保持现有的 `Ctrl ⇧ P` 写法。

### macOS 原生菜单

Tauri 在没有显式设菜单时会在 macOS 上装默认菜单（`Builder::enable_macos_default_menu` 默认开启），
其中有「关闭窗口 ⌘W」。菜单快捷键和网页的 keydown 谁先拿到要在 Mac 上实测，但不管谁先，
默认菜单的 ⌘W 都和「关闭标签页」打架。所以 macOS 上自建菜单：

- **保留 Edit 菜单。** WKWebView 里输入框的 ⌘C / ⌘V / ⌘X / ⌘A / ⌘Z 靠菜单分发，删掉菜单这些键全部失效。
- **⌘W 换成「关闭标签页」**，菜单项发事件给前端。前端 keydown 不再处理 `metaKey + W`，
  避免菜单和 keydown 各关一个标签页。
- 保留 ⌘Q、⌘H、全屏。

## 窗口外壳

### macOS 用原生窗口框

tao 在 macOS 上的 `drag_resize_window` 直接返回 `NotSupported`，`ResizeHandles.vue` 的 `catch` 把它吞掉了，
所以 Mac 上八个缩放热区按下去什么都不发生。热区盖在窗口边缘，会不会连系统自带的边缘缩放也挡住，要实测。

改为平台配置 `src-tauri/tauri.macos.conf.json`：`decorations: true`、`titleBarStyle: "Overlay"`、
`hiddenTitle: true`，红绿灯位置用 `trafficLightPosition` 对齐标题栏高度。平台配置按 JSON Merge Patch
合并，**数组整体替换**，`app.windows` 是数组，所以 Mac 那份要写出完整的窗口定义；改窗口尺寸、最小尺寸要改两份。

前端按 `isMac` 分支：

- `TitleBar.vue` 不渲染最小化 / 最大化 / 关闭三个按钮，设置按钮保留；左侧给红绿灯留位，宽度进 `tokens.css`。
- `ResizeHandles.vue` 不渲染，缩放交给原生窗口框。
- 拖动沿用 `data-tauri-drag-region`。Overlay 下网页盖住了原生标题栏，双击走的是 drag region 的切换最大化，
  不一定遵守系统设置里「双击标题栏」的选项；要实测，不遵守也能接受。

Windows / Linux 保持自绘。GNOME 的 Wayland 会话本来就不给服务端装饰，`startResizeDragging` 在 GTK 上可用。

### 关闭语义按平台分

| 平台 | 关闭 | 找回 |
|---|---|---|
| macOS | 红灯隐藏窗口，⌘Q 退出 | 点 Dock 图标（处理 `RunEvent::Reopen`）重新显示 |
| Windows / Linux | 退出应用 | 不需要 |

macOS 要拦住红灯触发的 `CloseRequested`（`prevent_close` 后 `hide`），否则窗口真的关掉、Tauri 随之退出。
Windows / Linux 的「关闭到托盘」需要先有托盘，作为单独的需求，不在本文范围；托盘出现之前，关闭就是退出。

## 凭据存储

### 读连接列表就会读密钥链

`connection_store::list_connections` → `summaries` → `secret_presence`：每个连接至少一次
`get_secret(RedisAuth)`，配了 SSH 或哨兵再多一到两次，**任何一次出错整个列表都失败**。
`save_connection` 先 `secret_snapshot` 读全部四个字段，没有密码的连接也一样。

- **Linux：** `keyring` 的 v1 模式在 Linux 上用 Secret Service（`zbus-secret-service-keyring-store`），
  需要会话里运行着 gnome-keyring 或 KWallet。精简的窗口管理器环境、部分发行版的最小安装没有它，
  结果是**连接列表加载失败、任何连接都存不下**，不只是「密码存不了」。
- **macOS：** 每读一个条目就是一次 Keychain 访问。应用签名变化时（未签名的开发构建、ad-hoc 签名、
  更换证书后的更新）系统可能逐条弹「允许访问」，启动时列表有 N 个连接就弹 N 次以上。要在 Mac 上实测。

### 初始化失败会粘住

`keyring::Entry::new`（v1）先把「已初始化」标志置位，再构造平台 store；构造失败时标志不回退，
此后进程里每次调用都返回 `NoDefaultStore`。用户事后启动或解锁了密钥服务，也得重启应用。
zbus store 是否在构造时就连 D-Bus 要在 Linux 上确认；如果是，「应用启动时密钥服务还没起来」也会触发。

### 设计

- **列表不碰密钥链。** 是否存了密码（`has*`）不是秘密，写进连接配置文件。只有打开连接、
  或保存时确实要改某个秘密字段时才访问密钥链；保存只读、写本次改动的字段。用户在系统钥匙串里
  手动删了条目、标志和实际脱节时，连接时按「没存密码」处理并提示。
- **初始化可重试。** 从 `keyring`（v1 便捷层）换成直接依赖 `keyring-core` 和各平台的 store crate，
  初始化由我们自己管：失败不缓存，下次调用重试。`keyring` 自己的文档也建议应用这样做。
  依赖变了要重跑 `gen-notices.mjs`。
- **密钥链不可用不等于连接不可用。** 保存配置照常成功；密码字段提示「系统密钥链不可用，密码不会被保存」，
  并提供「仅本次使用」：密码只随这一次打开连接传给后端，不落盘。`redis_open_connection` 已经接受完整配置，
  但这样打开的会话要能对上存储态连接的 key（改完配置作废已开会话依赖它），具体接法实现时定。
  错误沿用 `connection_store.secret_unavailable`，前端给可读的中文说明，Linux 上指明需要 gnome-keyring 或 KWallet。

## 格式化器

### GUI 进程的 PATH 不是终端的 PATH

`resolve_executable` 对裸命令名走 `find_executable_in_path`，读的是进程自己的 `PATH`。
macOS 上从 Finder / Dock 启动的应用继承的是 launchd 的最小 `PATH`（`/usr/bin:/bin:/usr/sbin:/sbin`），
没有 `/opt/homebrew/bin`、`/usr/local/bin`、`~/.cargo/bin` 或 npm 全局目录。用户在终端里能跑的 `jq`、`prettier`，
在应用里报 `formatter.command_missing`。Linux 视桌面环境而定，`~/.local/bin` 经常不在图形会话的 `PATH` 里。

设计：

- **找不到时的提示说清原因。** 非 Windows 平台上 `formatter.command_missing` 的提示写明「图形应用的 PATH
  与终端不同」，建议填绝对路径。绝对路径现在就支持（`path.is_absolute()` 分支）。
- **macOS 追加 Homebrew 目录。** 在进程 `PATH` 之后、只在 macOS 上追加 `/opt/homebrew/bin` 与 `/usr/local/bin`。
  放在末尾，不改变已有 `PATH` 里同名命令的优先级。

### unix 分支需要实际跑一次

`cfg(not(windows))` 的分支（`/bin/sh -c`、临时文件 `0o600`、测试里的 unix 命令变体）写了但没在 unix 上跑过。
实现本提案时至少在 Linux 上跑一次 `cargo test`。超时只杀直接子进程（`execute_prepared` 里的注释）
是三个平台共有的已知限制，不在本文范围。

## 落地去向

| 内容 | 并入 |
|---|---|
| 快捷键表、主修饰键规则、macOS 菜单 | `doc/frontend/tabs.md`（已覆盖 `useShortcuts.ts`），同时去掉「改一处要改两处」 |
| 窗口外壳、关闭语义 | 新建 `doc/frontend/window.md`，覆盖 `TitleBar.vue`、`ResizeHandles.vue`、`tauri.macos.conf.json`、`lib.rs` 的窗口事件 |
| 凭据存储 | 新建 `doc/backend/credentials.md`，覆盖 `credential_store.rs`、`connection_store.rs` 的秘密读写 |
| 格式化器的 PATH | 写进 `formatter.rs` 的代码注释；只涉及一个函数，不单独成文 |

新文档登记进 `doc/README.md` 的「代码路径 → 文档」对照表。

## 验证

- **前端单测：** 快捷键表在两种平台下的匹配与文案（`isMac` 可注入）；Mac 上 `⌘W` 在输入框里生效，CLI 的 `Ctrl W` 仍然删词。
- **后端单测：** 用 `MemoryCredentialStore` 的故障变体验证：密钥链全部报错时，列表与无密码连接的保存都成功；
  初始化失败后再次调用会重试。
- **实机：** Mac 上检查红绿灯、原生缩放、⌘C / ⌘V、⌘W、Dock 找回窗口、签名变化后的授权弹窗次数；
  Linux 上在没有 Secret Service 的会话里打开应用、保存连接、使用「仅本次使用」。

## 不做

- **密钥链不可用时明文回退。** 连接对话框承诺「密码不写进配置文件」，回退会无声打破这个承诺。
- **启动时执行登录 shell 读取 `PATH`**（`$SHELL -l -c 'echo $PATH'`）。它会跑用户的 rc 文件，
  慢的 rc 拖慢启动，有副作用的 rc 在每次启动时都执行一遍。
- **macOS 上把 `` Ctrl ` `` 改成 `` ⌘` ``。** 会占用系统的窗口切换键。
