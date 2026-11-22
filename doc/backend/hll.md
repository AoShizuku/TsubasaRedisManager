# HyperLogLog

覆盖：`src-tauri/src/services/hll.rs`、`src-tauri/src/services/values.rs`（`get_hll_count`、`add_hll_elements`、`inspect_hll`、`HLL_INSPECT_FETCH_LIMIT`）、`src-tauri/src/services/connection.rs`（`get_key_meta` 的 HLL 探针）、`src-tauri/src/services/readonly.rs`（`PFCOUNT`）、`src-tauri/src/models.rs`（`HllMeta`）

HLL 在 Redis 里就是 string：`TYPE` 返回 `string`，它只是一段带 16 字节头的字节串（头部布局见 `hll.rs`
的模块注释）。所以它在这里是 **string 的子形态**：`KeyMeta.hll: Option<HllMeta>`，`key_type` 不动。
查看器见 [frontend/hll-viewer](../frontend/hll-viewer.md)。

## 识别：打开 key 时 GETRANGE 探针

`get_key_meta` 的第二批 pipeline 在 `key_type == "string"` 时顺带一条 `GETRANGE key 0 15`，
**零额外往返**。解析在 `hll::detect`。

- **校验比服务端严一格**（`parse_header`）：长度够 16、魔数 `HYLL`、encoding ≤ 1、**保留字节全零**。
  服务端不查保留字节；多查这一项，压低碰巧以 `HYLL` 开头的普通字符串被误认的概率。
- **识别只看头部。** 身体坏掉的 HLL 照样识别成 HLL——要先进 HLL 查看器，才轮得到它如实降级
  （见「inspect：降级逐字段」）；在识别阶段就拒掉，它会落回可写的 StringViewer。
- 探针那一格被 ACL 或 `rename-command` 挡掉时读成空，按普通 string 处理（pipeline 的 `ignore_errors`）。

**SCAN 刻意不识别。** 给每个 string key 补一条探测，等于为一个罕见的子形态让全量扫描的成本翻倍；
`withTtl` 做成开关是同一个成本逻辑，而 HLL 连开关都不值得。树与列表的圆点因此一直是 string 色——
这不是妥协而是如实，Redis 自己就是这么看它的。

## 命令

命令以 `commands/redis.rs` 与 `values.rs` 的 HYPERLOGLOG 段为准：计数（`PFCOUNT`）、结构（`inspect_hll`）、
添加（`PFADD`，元素是 base64 字节，走 `writable_runtime`）。设计判断：

- **计数把服务端错误原样透传。** 非 HLL 值或损坏值上 `PFCOUNT` 会报错，前端靠这句措辞进回退态
  （[frontend/hll-viewer](../frontend/hll-viewer.md)「结构查看器」）。后端不替它翻译，翻译了前端就分不清
  「服务端说这不是 HLL」与「连接断了」。
- **原始字节视图复用 `redis_get_string_value`**，不另设入口：dense 恒为 12,304 B，远低于
  `DEFAULT_VALUE_LIMIT`，天然完整。
- **`inspect_hll` 只取一个窗口**（`HLL_INSPECT_FETCH_LIMIT`）：sparse 超过 `hll-sparse-max-bytes` 就会转 dense，
  正常的 HLL 远小于这个数。超出窗口的「HLL」不是正常 HLL，按降级处理，不把整个大值搬过 IPC。
- 没有 `PFMERGE` 命令，理由见 [frontend/hll-viewer](../frontend/hll-viewer.md)「不做」。

### PFCOUNT 的写语义

`PFCOUNT` 会回写头部 8 字节的基数缓存：**物理字节层面**它可能改这个 key（`MAY_REPLICATE`），
**数据语义层面**它永远不改变这个 HLL 代表的集合。Redis 的裁决按语义：ACL 归类 `@read`，只读副本可执行。

由此两条结论，各自站在正确的理由上：

- **只读会话放行 `PFCOUNT`**：计数走 `session_runtime` 而不是 `writable_runtime`，CLI 的 `readonly.rs`
  允许列表里也有它。只读守卫保护的是数据，不是服务器的每个内部字节；只读会话——恰恰是对着生产库的
  场景——打开 HLL key 必须能看到基数。
- **探针不用 `PFCOUNT`**（见「不做」）。那里问的是更严的问题：有零副作用的替代时，元信息读取没有理由
  选有副作用的。

这个副作用写在界面的状态行上，见 [frontend/hll-viewer](../frontend/hll-viewer.md)「仪表盘」。

## inspect：降级逐字段

6-bit 解包与 sparse 的 ZERO / XZERO / VAL 操作码是 Redis 实现知识，归 Rust（`hll::inspect`）。
前端拿到的是**展开成 16,384 个值的规范数组**——dense 与 sparse 在界面上因此没有分支，热力图对两者同样成立。

返回形状（`HllInspect`）按「能解析到哪一层」降级，与真实的损坏形态一一对应：

| 形态 | 服务端行为 | inspect 的降级 |
|---|---|---|
| 截断的 dense（长度 ≠ 12,304） | `WRONGTYPE Key is not a valid HyperLogLog string value` | 头部照常解析；前缀寄存器照常展开，`validRegisters` 划界，缺失段补零 |
| sparse 操作码坏 | 头部合法所以过校验，计数中途 `INVALIDOBJ Corrupted HLL object detected` | 头部照常解析；**不给寄存器**——半截操作码流无从知道哪段可信 |
| sparse 大于取回窗口 | — | 同上，不展开不完整的操作码流 |
| 伪魔数 | 各命令按普通 string 处理 | 识别阶段就拒掉，进不了 HLL 查看器；直接调 inspect 得到 `header` 为空 |

还有一档更隐蔽的：**坏 sparse + 基数缓存仍标有效**。`PFCOUNT` 直接返回缓存值、根本不走寄存器——
不报错，只给一个陈旧的数。这时查看器不进回退态（服务端没说它坏），但解析视图显示「寄存器无法展开」。
**计数正常 ≠ 数据正常**，所以结构查看器不是装饰。

e2e：`tests/redis_values.rs` 的 `corrupted_hyperloglogs_degrade_honestly`（构造损坏值时必须把缓存标成失效，
否则 `PFCOUNT` 会安静地返回缓存值）、`hyperloglog_full_path_detects_counts_and_inspects`。

## 不做

- **用 `PFCOUNT` 做识别探针**（成功即 HLL，还顺便拿到基数）。三个理由，按承重排序：
  1. **有纯读替代。** `GETRANGE` 16 字节、O(1)、永远有合法答案。
  2. **错误当常态。** 绝大多数 string 不是 HLL，探针以报错为常态，是拿错误当控制流。
  3. **成本不成比例。** 缓存失效时 `PFCOUNT` 全量扫 16,384 个寄存器（ACL 归类 `@slow`），还会回写基数缓存——
     元信息读取在有纯读替代时没有理由留下服务端痕迹。
- **在前端识别**（StringViewer 拿到值后看前缀切视图）。`HYLL` 魔数是 Redis 实现知识，
  [architecture](../architecture.md)「前后端分层」的「协议知识住在 Rust」直接否掉；而且得先把无用的二进制值
  拉过 IPC 才知道该换视图。
- **SCAN 时识别**，见「识别：打开 key 时 GETRANGE 探针」。
- **只读会话拦 `PFCOUNT`**，见「PFCOUNT 的写语义」。
