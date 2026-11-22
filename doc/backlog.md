# 需求池

未排期。每条先说**用户会撞上什么**，再说卡在哪——判断轻重不该需要先读代码。
排进版本后移到 [roadmap](roadmap.md)，有设计的写成提案；做完从这里删除。

## Codec 与值编辑

### msgpack 与 protobuf 只能一个一个手点

*某个 Redis 里所有 `cache:*` 都是 msgpack。每打开一个 key 都要手动点一次「编码 → MessagePack」。*

两条路都不通：这两个格式没有魔数，任意字节都可能是合法 msgpack，放进自动识别会把
所有值都认成它；而「这个连接的值默认按 msgpack 解」的设置不存在——`ConnectionConfig`
里没有 codec 字段。gzip 等有魔数的不受影响。

要做的是 codec 三层配置里的第 2 层（连接级默认，见 [backend/codec](backend/codec.md)「三层配置」）。
**只作用于 value，不作用于 key**——Spring 的典型配置是 key 明文、value 二进制，对 key 解码会把
key 名解坏。打开值时直接按默认 codec 解（压缩类可以在后端取值时就解好，省一次往返），
单 key 的手点覆盖照旧走 `CodecPicker`。**解不开必须回退原始字节并如实标注**，不能让取值失败——
一个配错的默认 codec 不该让整个 key 浏览器瘫掉（「硬规则」）；msgpack / protobuf 解析器
「解不开就抛」的严格性正是为这个用法准备的。连接级前缀规则可以一并考虑。

### 截断的 STRING 解不了码

*一个 400 KB 的 Java 序列化对象存在 STRING 里。打开它，编码菜单整个是灰的，提示值已截断；
横幅上没有「加载完整值」。*

STRING 按 `DEFAULT_VALUE_LIMIT` 截断，截断守卫（[frontend/codecs](frontend/codecs.md)「截断守卫」）
正确地拒绝解码，但 `StringViewer` 不提供下一步。后端已经具备：`redis_get_full_value` 的 `string` 变体走
窗口路径、上限 `FULL_VALUE_LIMIT`，只是没有前端调用方。要做的是在 `StringViewer` 的截断横幅上加
「加载完整值」，重取后以完整值重建链；重取回来仍超限时保持只读。

### 集合成员能解开看，但改不了

*hash 的 `user:1` 字段存着 gzip 的 JSON。抽屉里能解开看，但「编辑」是禁用的，提示「值不是 UTF-8」。*

抽屉里的解码和编辑对话框是两条互不相通的路：对话框读的是原始压缩字节，跟抽屉里
解出来的东西没有连接。STRING 有完整链路（解开 → 改 → 保存时沿链压回去），集合成员没有。

不会损坏数据——二进制与截断的值被编辑框挡着（[IPC 契约](backend/ipc-contract.md)「截断契约」），
所以是缺功能不是 bug。而 hash 字段恰恰是 msgpack / php-serialize 最常见的存放位置。

### 截断的 Set / ZSet 成员只能去 CLI 删

*一个 set 成员有 100 KB，删除按钮是灰的，提示成员被截断、请走 CLI。*

成员本身就是定位符，截断后拿不到完整字节，也没有别的定位符可以重取，所以界面禁用了删除。
要在界面里删，得换一种不需要完整字节的定位方式，例如按模式的批量删除任务（`SSCAN` / `ZSCAN` 在后端拿到
完整字节再删）。

### 序列化结构解出来只能看

*一个 msgpack 值解出来是 `{user: {id: 12345678901234567890, tags: [...]}}`，能看，编辑器是只读的。*

展示文本故意不是能读回结构的格式：做成 JSON 就等于「把结构压成文本再读回来」，
int64 精度、二进制字段、非字符串 map key 正是在那一步丢掉的。要做的是树形逐字段编辑；
三个可往返解析器的 `encode` 已经写好（现在只用于往返自检），保存路径的形状也已就位：
`RENDERER_CODECS[c].encode(node)` 拿到字节，再把链上**前缀**那几层压缩交给 `encodeAlongChain`。
pickle 与 Java 序列化永远不进这条路（安全边界），界面上靠 `Editable.kind` 说成另一句话。

两种只读要分清：「重编回去就不是原字节了」是数据的性质，做出编辑器也不能保存；
「结构完整可逆但没有编辑器」是工具的能力，这一条做完就能编。

## Stream

### Stream 里的字段值解不了码

*一条 stream entry 的 `payload` 字段存着 gzip 的 JSON。点开那条 entry，抽屉里是整条 entry
所有字段拼成的一坨文本，没有编码按钮。*

Stream 的选中粒度是条目而不是字段：抽屉拿到的是拼接文本，里面没有单个值可解，所以
`StreamViewer` 给 `ValueDrawer` 传 `decodable=false`，把 `CodecPicker` 关掉了。Hash 的抽屉拿到的是单个值，
不受影响。要做的是前端的字段级选中（抽屉里放字段小表，或表格行展开）。
字段值在后端与 Hash 走同一条路径（`values::pairs_to_entries`），同样按 `MEMBER_VALUE_LIMIT` 截断，
所以字段级选中之后还要接重取。
后端很便宜：定位符 `key + entry id + field` 都现成且不截断，给 `ValueRef` 加一个
`StreamField` 变体即可，实现是 `XRANGE key <id> <id> COUNT 1` 后在后端挑字段。
Stream 是 append-only，只要解码不要编码。注意 `XRANGE` 是 entry 粒度的，
`FULL_VALUE_LIMIT` 对 Stream 只保护 IPC 不保护网络。

### Stream 没有写路径

*想往 stream 里追加一条测试消息、删掉几条坏消息，只能去 CLI。*

分三层：

- **v1**：字段级选中与解码（上一条）、`XADD`、勾选后 `XDEL`、`XTRIM`。已有 entry 不提供「编辑」，
  Redis 只能新增或删除。
- **v2**：`XINFO CONSUMERS`、`XPENDING` 明细、创建 / 删除消费组、设置 group ID、删除消费者。
- **v3**：恢复与清理工作流：`XACK`、`XAUTOCLAIM`、Redis 8.2 的 `XACKDEL` / `XDELEX`、8.8 的 `XNACK`。
  必须清楚标明会改变 PEL 或删除消息。

`XADD` 高级选项按能力开放：8.2 引用清理策略；8.6 `IDMP` / `IDMPAUTO` 幂等生产；
如果以后提供实时 `XREAD` / `XREADGROUP`，8.10 的 `MAXCOUNT` / `MAXSIZE` 限制总回复。
参考：[Stream 幂等](https://redis.io/docs/latest/develop/data-types/streams/idempotency/)、
[XREADGROUP](https://redis.io/docs/latest/commands/xreadgroup/)

## Bitmap

*想看一个用作签到表的 string 里第 10086 位是不是 1，只能去 CLI 敲 `GETBIT`。*

Redis 官方明确 Bitmap 不是独立类型，而是对 String 的位操作解释
（[文档](https://redis.io/docs/latest/develop/data-types/bitmaps/)）。做成 StringViewer 的「位图」模式：

- 不自动识别——任意字符串都能解释成 bitmap，没有可靠魔数。
- 分页读取小窗口（512–4096 字节）；不能把 256 KiB 窗口直接渲染成两百多万个 bit DOM。
- 行首显示 byte / bit offset，支持跳转。
- 当前范围 `BITCOUNT`、查找下一个 0/1 的 `BITPOS`；全量 `BITCOUNT` 对大 key 可能阻塞，显式触发或走任务。
- 单 bit 编辑走 `SETBIT`；整数段读写走 `BITFIELD_RO` / `BITFIELD`。
- 多 key `BITOP` 做成单独的「位图计算器」（它会写目标 key）；Redis 8.2 的 `DIFF` / `DIFF1` / `ANDOR` / `ONE`
  按能力开放（[8.2](https://redis.io/docs/latest/develop/whats-new/8-2/)）。

同一思路以后可用于 ZSet 的 Geo 子模式。

## Redis 8 新类型与能力

不做成一个「Redis 8 支持」开关，按能力逐项开放：每个功能连同自己的能力登记项一起提交，
见 [backend/capabilities](backend/capabilities.md)「新增一项能力」。8.0 整合了 JSON、Time Series、
Search、Vector Set 和五种概率结构；8.8 增加 Array
（[8.0](https://redis.io/docs/latest/develop/whats-new/8-0/)、[8.8](https://redis.io/docs/latest/develop/whats-new/8-8/)、
[8.10](https://redis.io/docs/latest/develop/whats-new/8-10/)）。

| 优先级 | 能力 | 最小可用范围 |
|---|---|---|
| 高 | JSON | 懒加载树、JSONPath、路径级读写，避免整文档搬运 |
| 高 | Hash 字段 TTL | 每行 TTL、批量 `HTTL` / `HEXPIRE` / `HPERSIST`、保存时 `HSETEX` |
| 中高 | Time Series | 范围查询、降采样、标签 / 规则、折线图、追加样本 |
| 中 | Vector Set | 成员、维度 / 属性、相似搜索、添加与删除 |
| 中低 | Array | 稀疏下标浏览与 `ARSCAN`，不能复用 List 分页语义 |
| 低 | Bloom / Cuckoo / CMS / TopK / TDigest | INFO + 查询 / 写入面板；多数结构无法枚举原始成员 |

Search 不属于单 Key 查看器，应独立做成「索引与查询 Workbench」：索引列表、schema、查询、聚合、profile。

### STRING 保存缺并发保护

*两个人同时改同一个 STRING，后保存的人静默覆盖前一个人的修改。*

Redis 8.4 起可以用 `DIGEST` + `SET IFDEQ` 做条件写
（[条件写](https://redis.io/docs/latest/develop/using-commands/transactions/)），按能力开放。

## Key 树

### 浏览态续扫只能手点

*一个两百万 key 的库，想多看一些，就得滚到整棵树最底部点「加载更多」，每点一次来一批，然后再滚到底再点。*

「加载更多」只有一行，挂在整个 DB 子树的最底部（`sidebarRows.ts` 的 `more` 行），展开几个文件夹之后它就在视口外几千行。
要做的是 `more` 行进入虚拟化视口时自动触发一次 `scanMore`，把点击换成可见性，点击保留。
它只省点击，不承诺完整性——完整性靠搜索与「扫描此前缀」，否决过的两个更激进的方案见
[frontend/key-tree](frontend/key-tree.md)「不做」。

没有技术卡点，纯前端、改动小。要守住两条：触到 `treeNodeLimit`（`Session.truncated`）之后不再自动触发——
那里要求用户显式继续；`scanning` 在途时不重复触发（`scanMore` 自己的守卫已经挡住）。

### 扫描结果流入时树会跳

*搜索结果还在流进来，正盯着第 40 行的 key 准备点，它突然被挤到下面去了；点完「加载更多」，刚才看的那一片也换了位置。*

新 key 按排序插进树的任意位置，增量到达还会让紧凑合并行裂开（[frontend/key-tree](frontend/key-tree.md)「增量到达会改变合并结果」）。
行状态已经按完整前缀存，裂开不会丢展开态；缺的是滚动位置：`KeyTree.vue` 的虚拟列表按行 id 认行，但重建后
不做滚动锚定。要做的是重建前记下视口首行（或光标行）的 id，重建后按 id 找回新下标再滚过去。
对仍在加载的分支延后合并是另一条路，代价是合并结果要等扫完才稳定，优先级在锚定之后。

## TTL

### TTL 读数只在本地倒数

*同事在另一个客户端给 `session:42` 加了 PERSIST，切回窗口，chip 还在倒数；倒数归零后 chip 变成「已过期」，
下面的表却还是原来的值，树上那一行也照常显示。*

倒计时按设计在本地递减（[frontend/ttl](frontend/ttl.md)「倒计时在本地递减，不轮询服务端」），但重同步时机里没有
「窗口重新获得焦点」，别处的修改要手点刷新才看得到。归零也只改 chip：`KeyTab.vue` 只在取元信息时判 `missing`，
不会在本地倒计时到 0 时自己重取，所以失效态要等下一次重同步；`tree.css` 里给失效节点准备的 `.trow.is-stale`
也没有任何地方用到。要做的是窗口 `focus` 时重取活动 key 标签页的元信息，倒计时到 0 时重取一次
（返回 `-2` 就进失效态），并把失效的 key 在树上标灰。

## 标签页与入口

### 导入 / 导出没有界面

*点 `+` 菜单的「导入 / 导出…」，只弹出一句「请在 Key 树的右键菜单中选择目标范围」，而右键菜单里没有这一项。*

后端任务都在（`redis_start_import_keys`、`redis_start_import_commands`、`redis_start_export_keys`，
`api/redis.ts` 里也有封装），前端没有调用方。要做的是导入 / 导出对话框：导出的作用范围可以照搬批量对话框
（显式列表或模式，见 [frontend/batch](frontend/batch.md)「作用范围」），树里 namespace 右键加「导出…」；
导入要选文件，只读会话下禁用（`+` 菜单那一项已经按只读置灰）。修好之前，至少别让那句提示指向一个不存在的菜单项。

### 搜索结果没法一键带进 Key 浏览器

*侧栏搜 `order` 出来几千个 key，想按内存排一下，只能开一个 Key 浏览器再把 `*order*` 手敲一遍。*

上下文入口里少了这一条（[frontend/tabs](frontend/tabs.md)「上下文入口：从树里带着上下文进去」）。要做的是搜索态
下在底部行或搜索框旁给「在 Key 浏览器中查看」，模式用会话当前的搜索模式（`Session.pattern`，含前缀作用域）。
纯前端，`tabs.open({ kind: 'browser', pattern })` 已经支持。

## 批量操作

### 批量 TTL 中途失败后，已打开的标签页读数不刷新

*批量给 `cache:*` 设 TTL，跑到一半连接抖了一下，任务报失败。开着的 `cache:page:home` 标签页上
TTL chip 仍显示「不过期」，其实它已经被设上了。*

`BatchTtlDialog` 的 `report` 在 `failed` 时直接返回，没有调 `sessions.invalidateKeyspace`；
`completed` 与 `cancelled` 都调了。批量删除三种结局都刷（失败之前删掉的也是真删了），批量 TTL
同理应该刷。见 [frontend/batch](frontend/batch.md)「结果反馈」。

## 新建与克隆

### 克隆只能一个一个来，大 Key 克隆不了

*想把 `session:*` 整片复制到测试环境，只能逐个右键「克隆 Key…」；其中一个 80 MiB 的 key 直接被拒，
提示超过单 Key 上限。*

单 Key 克隆把整份 `DUMP` 放进内存（`DUMP` 没有范围读取），所以有 `MAX_DUMP_BYTES` 上限，
全应用也只允许一个在途传输（`TRANSFER_SLOT`），见 [backend/key-write](backend/key-write.md)「大值边界」。
批量克隆要做成任务：模式扫描、逐 key 快照与恢复（集群下的落点见 [backend/cluster](backend/cluster.md)
「批量写：按节点整批或逐 key」）、进度口径，以及同名冲突是整体停止还是跳过并计数——单 Key 的「同名即停」不能直接照搬。
更大的值没有原生的分块快照，需要另外设计迁移路径。

## 诊断

- **Slow Log + `HOTKEYS`（8.6）+ 集群 `CLUSTER SLOT-STATS`**——比持续开 MONITOR 更适合日常诊断
  （[HOTKEYS](https://redis.io/docs/latest/commands/hotkeys/)、[SLOT-STATS](https://redis.io/docs/latest/commands/cluster-slot-stats/)）。

## 会话与连接

### CLI 里的 SELECT 会让整个会话换库

*在可写连接的 CLI 里敲 `SELECT 3`，侧栏仍显示 db0，但之后打开的 key、保存的值都落在 db3。*

会话是 DB 级的，前端按 `(连接, db)` 记账（[sessions](backend/sessions.md)「会话是 DB 级的」），
而 CLI 与功能路径共用同一个会话 executor。`SELECT` 不在 `readonly.rs` 的允许列表里，所以只读会话
会拦；可写会话不拦，切库对整个会话生效，直到连接重建才回到配置的 db。
要做的是在 `execute_cli` 里拒绝 `SELECT`（并提示「切库请在侧栏展开对应 DB」）。
此条由读代码得出，未实测。

### 非 UTF-8 的 key 名扫不出来

*库里只要有一个二进制 key 名，侧栏那一页就报错，同一页里的其他 key 也看不到。*

IPC 上 key 名是 `String`，`connection::parse_scan_response` 遇到非 UTF-8 的 key 直接让整页失败。
要做的是 key 名也改成字节表示（同 `RedisText` 的思路），牵涉所有以 key 为参数的命令。

## 其他

- **订阅标签页静默丢消息**：环形上限以外的消息被丢弃，但不像 Monitor 那样显示丢了多少
  （`PubSubTab.vue` 没有渲染 `useTaskStream` 的 `dropped`）。
- **深色主题**：设置里的开关是灰的。`tokens.css` 只有浅色一套令牌；颜色都走变量，
  加主题主要是补一套覆盖。
- **CLI 补全与语法提示**：基于运行时 `COMMAND DOCS`，覆盖 8.x 长尾命令，同时避免把第三方命令文档复制进仓库。
- **Pub/Sub 发布**：现在只有订阅。
- **Key 对比 / 分割视图**：比较 JSON、Hash 和不同环境。
- **e2e 版本矩阵**：现在跑 `redis:7-alpine`（另有 `redis:6-alpine` 供克隆兼容性用例）。
  保留 Redis 7 基线，增加 Redis 8.x 单机 / 集群；做 JSON、Time Series 时再加 Redis Stack 7.x。
