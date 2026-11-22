<div align="center">

<img src="src-tauri/icons/128x128@2x.png" width="96" alt="Tsubasa Redis Manager" />

# Tsubasa Redis Manager

**面向大 keyspace 与生产实例的 Redis 桌面客户端**

[![CI](https://github.com/AoShizuku/TsubasaRedisManager/actions/workflows/ci.yml/badge.svg)](https://github.com/AoShizuku/TsubasaRedisManager/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platform: Windows](https://img.shields.io/badge/platform-Windows-0078d4.svg)

</div>

> A Redis desktop client built with Vue 3 and Tauri v2. It keeps the key tree and tables
> virtualized for large keyspaces, makes read-only a property of the session rather than a UI
> hint, and decodes common formats (gzip, Brotli, MessagePack, Protobuf, PHP, pickle, Java)
> without instantiating anything. The UI is in Chinese.

> [!WARNING]
> **项目正在迭代，尚未稳定。** 0.x 版本之间的功能、界面与本地配置格式都可能变化；
> 连接生产实例前建议勾选「只读」。目前只在 Windows 上构建与测试。

## 功能

- **连接**：单机、集群、Sentinel，TLS 与 SSH 隧道；密码存系统凭据管理器，可从 ARDM 导入连接
- **Key 树**：命名空间折叠与紧凑路径、虚拟化渲染、按前缀扫描与搜索，计数随扫描逐步收敛
- **查看与编辑**：String、Hash、List、Set、ZSet、Stream、HyperLogLog 分类型查看器，按各类型真实的分页方式取值；
  大值由后端截断并在界面上只读，不会因保存截断值而丢数据
- **解码链**：gzip、Deflate、Brotli 压缩与 MessagePack、Protobuf、PHP serialize、pickle、Java 序列化；
  pickle 与 Java 只做结构解析，不加载类、不实例化对象
- **只读会话**：只读在打开会话时定死，所有写路径在后端入口拦截，不依赖界面禁用
- **批量任务**：扫描、批量删除、批量 TTL（支持试算）、导入导出、跨连接克隆，进度可取消
- **工具**：CLI、MONITOR、Pub/Sub、内存分析；按服务端能力（版本、参数、模块）自动启用或说明原因

## 构建

需要 Node.js 22 与 Rust stable，以及 [Tauri v2 的系统依赖](https://v2.tauri.app/start/prerequisites/)。

```powershell
npm ci
npm run tauri:dev     # 开发模式
npm run tauri:build   # 出安装包
npm run check         # 前端构建 + 测试 + clippy + cargo test，不需要 Redis
```

e2e 需要本地 Docker 起 Redis（单机、集群与能力矩阵），见 [开发手册](doc/development.md)。

## 文档

从 [doc/README.md](doc/README.md) 进入：前后端设计分别在 `doc/frontend/` 与 `doc/backend/`，
描述的是当前代码，与代码同一个提交修改。`doc/fe_ref/` 是静态 HTML 原型，直接打开 `index.html` 即可。

参与贡献前请读 [CONTRIBUTING.md](CONTRIBUTING.md)；安全问题请按 [SECURITY.md](SECURITY.md) 私下报告。

## 已知限制

- SSH 隧道暂不支持 RSA 私钥：所用 Rust 密码学依赖的 RUSTSEC-2023-0071 尚无修复版本
- SSH 隧道暂不校验主机指纹（尚未接入 known_hosts），见 [SECURITY.md](SECURITY.md)
- 只有浅色主题

## 来源与许可

本项目以 [MIT 许可](LICENSE) 发布。它受 AnotherRedisDesktopManager 启发，但代码完全独立编写，
边界见 [PROVENANCE.md](PROVENANCE.md) 与 [ACKNOWLEDGEMENTS.md](ACKNOWLEDGEMENTS.md)。

随程序分发的第三方组件保留各自的许可：[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) 是清单，
[THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt) 是许可证全文，两者都由
`node scripts/dev/gen-notices.mjs` 生成。`monaco-editor` 附带的 Codicon 字体为 CC BY 4.0，
其署名显示在程序的「设置 → 关于」中。
