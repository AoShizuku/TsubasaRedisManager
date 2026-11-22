# 更新日志

本项目的重要变更记录在此文件。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。0.x 期间次版本号之间可能有不兼容变更。

## [Unreleased]

## [0.1.0] - 2026-11-22

首个公开版本。项目仍在迭代，尚未稳定。

### 连接与会话

- 单机、集群、Sentinel；TLS（含客户端证书）与 SSH 隧道
- 连接密码、SSH 口令与私钥口令存系统凭据管理器
- 从 ARDM 的连接配置导入
- 只读按会话生效，写路径在后端入口拦截；会话按 DB 划分
- 按服务端能力（版本、参数、模块）启用功能，不可用时给出原因；兼容 Valkey、KeyDB、Dragonfly

### Key 浏览

- 虚拟化 Key 树：命名空间折叠、紧凑路径、缩进引导线、TTL 标记
- 按前缀扫描与搜索，计数随扫描逐步收敛
- Key 浏览器：可排序的虚拟化表格、多选与内存分析

### 查看与编辑

- String、Hash、List、Set、ZSet、Stream（只读）、HyperLogLog 分类型查看器，各按自己的分页语义取值
- 大值由后端截断，截断后编辑器只读
- 新建与克隆 Key（含跨连接），TTL 查看与修改
- 解码链：gzip、Deflate、Brotli、MessagePack、Protobuf、PHP serialize；pickle 与 Java 序列化只读解析

### 批量与工具

- 批量删除、批量 TTL，均支持试算
- 导入导出、内存分析，进度可取消
- CLI、MONITOR、Pub/Sub

[Unreleased]: https://github.com/AoShizuku/TsubasaRedisManager/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/AoShizuku/TsubasaRedisManager/releases/tag/v0.1.0
