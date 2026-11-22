# 路线图

只放接下来一两个版本。未排期的需求在 [backlog](backlog.md)；有设计的条目链接到 [proposals/](proposals/)。

## 下一版

按顺序：

1. **Stream 写路径 v1**——字段级选中与 codec 解码、`XADD`、勾选后 `XDEL`、`XTRIM`。
   见 backlog「Stream」。
2. **Hash 字段 TTL**——随功能登记能力 `hashFieldTtl`（`Command(HEXPIRE)`，未知时禁用），
   见 [backend/capabilities](backend/capabilities.md)「新增一项能力」与 backlog「Redis 8 新类型与能力」。

## 再下一版

- **Bitmap 模式**——STRING 的显式查看模式，不是第七种类型。见 backlog「Bitmap」。
- **Redis 8 新类型从 JSON 开始**。见 backlog「Redis 8 新类型与能力」。
