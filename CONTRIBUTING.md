# 参与贡献

感谢你愿意改进 Tsubasa Redis Manager！提交前请花几分钟读完本文。

## 开始之前

- **Bug**：请附上复现步骤、期望与实际行为、版本号（设置 → 关于）、服务端类型与版本
  （Redis / Valkey / KeyDB / Dragonfly，单机 / 集群 / Sentinel）。
  日志或截图中请先去掉主机地址、密码、key 名里的业务数据等敏感信息。
- **功能**：较大的改动请先开 Issue 讨论方向，避免做完才发现与设计冲突。
  未排期的需求在 [doc/backlog.md](doc/backlog.md)，近期计划在 [doc/roadmap.md](doc/roadmap.md)。
- **安全问题**：不要公开提 Issue，请按 [SECURITY.md](SECURITY.md) 私下报告。

## 开发环境

需要 Node.js 22、rustup（Rust 版本由 `rust-toolchain.toml` 钉住，自动安装）与 [Tauri v2 的系统依赖](https://v2.tauri.app/start/prerequisites/)。
目前只在 Windows 上开发与测试。

```powershell
npm ci
npm run tauri:dev
```

测试分层、e2e 与许可清单见 [开发手册](doc/development.md)；模块边界见 [技术架构](doc/architecture.md)；
修改某个功能前先读 [文档索引](doc/README.md) 中对应的专题设计。

## 测试

- PR 前请确保 `npm run check` 全绿（前端构建与类型检查、vitest、rustfmt、clippy、cargo test），
  它不需要 Redis。CI 跑的就是这一套。
- 改动 `src-tauri/src/services/values.rs` 或连接层后，请在本地跑一次 e2e（`./scripts/dev/e2e.ps1`，需要 Docker）。
  e2e 用例默认 `#[ignore]`，不要去掉。
- 集群相关的改动单机测不出来，请说明你在什么拓扑上验证过。
- 增删或升级运行时依赖后运行 `node scripts/dev/gen-notices.mjs`，随依赖改动一起提交。

## 不可破坏的约束

完整清单见 [CLAUDE.md](CLAUDE.md)，PR 不应削弱这些：

- **来源**：不要把第三方源码复制进仓库；不要复制、移植或改编 ARDM 的前端代码。见 [PROVENANCE.md](PROVENANCE.md)。
- **前端不发裸 Redis 命令**（CLI 标签页除外），功能走后端的语义化命令或任务。
- **截断的值只读**：`totalBytes` 非空时编辑器必须只读，保存截断值会静默丢数据。
- **只读属于会话**：写命令在后端入口按会话状态拦截，不靠界面禁用。
- **String 保存不退回裸 `SET`**（会清 TTL）；pickle 与 Java 序列化只解析、不编码、不实例化。
- 依赖服务端版本或模块的功能走能力注册表，不按 `redis_version` 判断。

## 代码与界面约定

- 不能用 TypeScript `enum` 与参数属性（`erasableSyntaxOnly`），改用 `as const` 对象 + 联合类型。
- 颜色、尺寸、字号走 `src/styles/tokens.css` 的变量；图标先在 `src/lib/icons.ts` 登记。
- Redis 红只用于品牌与危险，强调一律用蓝色；等宽字体只给数据。见
  [visual-language](doc/frontend/visual-language.md)。
- 文档描述系统现在的样子，与代码同一个提交修改；实现过程写进提交信息，不进文档。

## 提交与 PR

- 一个 PR 做一件事；提交信息使用 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/)
  风格，如 `feat: …`、`fix(cluster): …`、`docs: …`、`test: …`、`chore: …`，中英文均可。
- 用户可见的变化请在 [CHANGELOG.md](CHANGELOG.md) 的 `Unreleased` 小节补一行。
- 界面改动请附截图。

提交即表示你同意以 [MIT 许可](LICENSE) 发布你的贡献。
