# 任务

覆盖：`src-tauri/src/services/task.rs`（批量删除、批量 TTL、试算；扫描任务：`run_scan_task`、`scan_pacing_delay`、`SCAN_TASK_PACING_CAP`、`ScanProgressEstimator`、`scan_and_annotate_one_page`、`normalize_scan_count`、`normalize_scan_pattern`）、`src-tauri/src/models.rs`（`BatchDeleteTaskOptions`、`BatchTtlTaskOptions`、`TaskEventItem::Key`、`estimated_progress`）、`src/components/shell/StatusBar.vue`（`TASK_TYPE_LABEL`、近似进度）

长耗时、可取消、带进度的操作是**任务**：`redis_start_*` 注册一个任务、立即返回 `task_id`，
之后的进度与结局都走 `redis://task-progress` 事件，快照是 `TaskStatusSnapshot`。
界面上的批量对话框见 [frontend/batch](../frontend/batch.md)，集群下怎么发见 [cluster](cluster.md)。

## 任务不是命令

批量删除、批量 TTL 是 task type（`batch_delete`、`batch_ttl`），不是语义化命令。命令层是
「一个用户动作 = 一个命令」的映射（[ipc-contract](ipc-contract.md)「一个用户动作一个命令」）；
批量要扫描、分块、可取消、报进度，它的正当形状是任务。往命令清单里塞一个「批量」，
命令层「不做业务编排」那条线就松了。

两个批量任务的参数同构（`BatchDeleteTaskOptions` / `BatchTtlTaskOptions`）：`keys` 是勾选来的显式列表，
`patterns` 是匹配模式，对应界面上的两种作用范围；两者都为空时拒绝启动（`TaskInvalidRequest`）。

## 进度口径

所有任务共用 `TaskStatusSnapshot` 的三个计数 `processed` / `succeeded` / `failed`。
**不为某一个任务加字段去污染所有任务**，所以每个任务要把自己的语义映射到这三个数上，
而且界面上的数字必须对得上。

### 批量 TTL：未变更是第三态

`EXPIRE` / `EXPIREAT` / `PERSIST` 回 `1` 表示真的改了，回 `0` 表示没改——key 不存在、条件没满足，
或者 `PERSIST` 时它本来就没有 TTL。**`0` 既不是成功也不是失败**（`TtlReplyOutcome`）：

| 字段 | 含义 |
|---|---|
| `processed` | 尝试过的 key 数 |
| `succeeded` | 回 `1`——真的改了 |
| `failed` | 报错，或那一格读不出整数 |
| `processed - succeeded - failed` | **未变更**，前端自己算 |

不这么分的话，勾了 100 个看到「成功 87」会让人以为漏了 13 个，然后去翻日志找一个不存在的 bug。
所以完成摘要（`batch_ttl_completed_message`）与前端 toast 都把「未变更」写出来。

单条被拒（ACL、`rename-command`）记一次 `failed`，不中止任务：pipeline 开着 `ignore_errors`，
那一格读成 `None`；读不出的格子宁可算失败，也不把没设上的算成设上了（`ttl_reply_outcome`）。

### 批量删除：只报 succeeded

- `succeeded` 是 `DEL` 真正删掉的个数。
- `failed` 恒为 0：单条 `DEL` 失败会让整个任务判失败并中止。pipeline 那条路也保持这个语义——
  有一格读不出整数就整批返回 `Err`（`delete_key_chunk`）。
- `processed - succeeded` 是「扫到时还在、发 `DEL` 时已经不在了」（自然过期，或者别人先删了）。
  **这个差值不报。** 它与批量 TTL 的「未变更」不同：那里差值代表条件没命中，用户可能要改条件重来；
  这里那些 key 现在确实不在库里，用户想做的事已经做到了。界面不显示 `processed`，说了反而凭空造出
  一个要去查的疑问。真跑的完成摘要因此为空（`batch_delete_completed_message`）。

### 扫描任务：只报命中数

`search_keys` / `load_all_keys` 的 `processed` 与 `succeeded` 都是**已返回的 key 数**，`total` 为空。
MATCH 模式下服务端只回命中的 key，**数返回值拿不到「已检查了多少个」**，所以不承诺一个拿不出来的分母：
界面上是「已找到 N 个」加不定进度。浏览态没有这个问题——不带 MATCH 时返回的就是全部 key，
「已加载 N / DBSIZE」是精确比值，见 [frontend/key-tree](../frontend/key-tree.md)「诚实信号」。

近似百分比走单独的 `estimated_progress`：只有扫描任务写它，其余任务恒为空，前端据此不显示百分比。
它由 `ScanProgressEstimator` 从 SCAN 游标估算：

- SCAN 游标是反转二进制迭代：把低 `bits` 位反转后随扫描单调递增，`rev_bits(cursor) / 2^bits` 即近似进度。
  位宽不用问服务端，首个非零游标的位宽就是表的位宽，扩表时观测到更宽的游标再抬高。
- 集群下对合成游标里的每个节点独立估算，按节点 DBSIZE 加权；合成游标里缺席的节点已扫完，计 100%。
  权重在首页之后的页间隙逐节点短持锁取一次——放进首页那次持锁交互，会把 N 个 RTT 串起来挡住同会话的交互命令。
  任一节点的 DBSIZE 取不到（ACL、`rename-command`）就**整体**退化为等权：真权重与凑出来的权重混在一起，比全部等权更歪。
- 失真来源：rehash 期间游标抖动、大批删除后表大小与 DBSIZE 脱节、集群权重只取一次。对外做单调不减（任务内高水位），
  抖动不外泄。**显示必须带「约」**，前端集中在 `formatApproxPercent`。
- **空页也发事件。** MATCH 稀疏时整页可能一个不中，这时只有估算在前进，不发的话「约 P%」就停住。
  有页间节流，本机全空页的最坏频率也只有每秒几百条，低于 Monitor。

e2e：`tests/redis_phase3a.rs` 的 `search_keys_task_page_loop_annotates_types_and_ttl_from_local_redis` 与
`tests/redis_phase2.rs` 的 `redis_cluster_scan_reads_keys_from_all_masters` 把真实服务端的游标喂进估算器，
断言单调不减、扫完收在 1.0。

## 试算

试算 = **同一个任务、同一份参数**，只是 `dryRun: true`：走完全相同的模式解析、扫描、分块路径，
只在最后一步不发写命令（`delete_chunk_for_connection` / `apply_ttl_for_connection` 开头的 `dry_run` 分支），
`processed` 照常累加命中数，`succeeded` 保持 0，完成摘要是「Matched N keys」。

用另一条扫描路径去数（比如 `search_keys` 任务）是错的：那是另一套扫描路径，口径会有细微差别，
而试算的**全部价值**就在于「试算出来的数字就是真会改的数字」。后端成本是一个 `if`。

- 参数校验照跑：批量 TTL 的试算同样过 `validate_ttl_spec`，参数无效时什么都不发。
- 只读会话上允许试算：它不写任何东西，「先看看影响多少」正是只读连接上会做的事。拒绝的是写，
  不是这个任务。见 [sessions](sessions.md)「只读属于会话」。

e2e：`tests/redis_phase5b.rs` 的 `dry_run_counts_without_deleting_anything`（试算后每个 key 都还在，
同一批在真跑时被删掉）、`tests/redis_phase5a.rs` 的 `batch_ttl_dry_run_counts_without_touching_anything`。

### 试算是独立的任务类型

试算注册成 `batch_delete_dry_run` / `batch_ttl_dry_run`（`batch_task_type`），前端 `TASK_TYPE_LABEL`
给它们配「（试算）」。共用一个类型的话，点一下「试算」，状态栏就亮起「批量删除 正在运行」——
那一秒用户会以为自己点错按钮把库删了。代价是一个 `format!`。

## 模式原样

批量任务的模式**原样使用**（`batch_scan_pattern`）：只 trim，不补 `*`，两个批量任务共用这一份。

> **界面上写的模式必须就是真正执行的模式。**

补 `*` 对 `matchPatternFor` 的产出无害（补成 `**`，`stringmatchlen` 会折叠连续星号），但对**手输**的模式
是静默扩大范围：想精确删掉 `cache:page:home` 这一个 key，真正执行的是 `cache:page:home*`，
`home:v2` 与 `homepage` 一起没了。这是「不要编数字」的另一面——也不要编模式。
e2e：`tests/redis_phase5b.rs` 的 `a_pattern_without_a_wildcard_deletes_exactly_one_key`。

**空模式丢弃**：空字符串会被 SCAN 当成「不带 MATCH」，那等于对全库动手，不该由一个没填的输入框触发。

模式里的 glob 元字符由谁转义见 [frontend/key-tree](../frontend/key-tree.md)「前缀生成模式必须转义 glob 元字符」：
从树的前缀生成时前端已经转义，手输的不转义，后端一律不碰。

## 批量 TTL 的后端校验

`start_batch_ttl_task` 在注册任务之前过 `validate_ttl_spec`，拒绝三种形状：

- 相对 TTL 小于 1 秒——`EXPIRE key 0` 立刻删掉 key；
- 绝对时刻不晚于现在——`EXPIREAT` 一个过去的时间戳同样是立刻删除；
- 带条件的 `PERSIST`——它没有条件参数。

前端已经拦了前两种，**后端必须再拦一次**：前端拦不住的路径（以后的脚本入口、被改坏的前端）
会变成一次静默的百万级删除。

## 扫到底的扫描任务

`search_keys`（搜索框、「在此搜索」）与 `load_all_keys`（「扫描此前缀」）是同一个循环 `run_scan_task`，只差任务类型：
逐页 SCAN 直到游标归零，每页结果经 `redis://task-progress` 流式回传。前端怎么消费见
[frontend/key-tree](../frontend/key-tree.md)「搜索」。

- **与会话共享 executor，按页加锁。** 交互命令在页间隙插队，干扰以一页为界。
- **暂停在页间、锁外等待**（`wait_while_paused`）：暂停中的任务不挡交互命令，等待期间取消照常生效。
- **取消令牌在页前、页后各查一次。** 一页 SCAN 服务端是毫秒级，在途的那页等它回来丢掉即可。
- **模式原样**：`normalize_scan_pattern` 只把空白归一成「不带 MATCH」，不补 `*`。搜索框的 `*词*` 包裹是前端的显式变换。
  扫描只读，所以空模式在这里就是扫全库，不像批量任务那样丢弃。
- **COUNT 独立于浏览路径。** 没给或给 0 时取 `normalize_scan_count` 的默认值；前端总是传设置项 `taskScanCount`，
  不用浏览路径的 `scanCount`。带 MATCH 时命中稀疏、回包小，大 COUNT 纯粹省往返，**不要把它调低到浏览路径的量级**。

### 事件项带 keyType

树的契约是「SCAN 返回的 key 自带类型」，任务路径与取值路径（`redis_scan_keys`）必须给出同一形状。
`scan_and_annotate_one_page` 逐页复用 `connection::annotate_keys`，在同一次持锁交互里 pipeline 一批 `TYPE`
（`withTtl` 时 `TTL` 同批），事件项 `TaskEventItem::Key` 带 `keyType`。集群下每批 key 来自同一个主节点，
注解 pipeline 整批打回那个节点，见 [cluster](cluster.md)「Pipeline 必须说明落点」。
单格被 ACL 挡掉降级成空，不影响别的 key；整批失败说明连接层坏了，任务判失败。

e2e：`tests/redis_phase3a.rs` 的 `search_keys_task_page_loop_annotates_types_and_ttl_from_local_redis`。

### 节流：按占空比自适应

扫到底的总工作量与 `KEYS` 相同，但形态不同：`KEYS` 一条命令把事件循环挡住整个扫描时长，所有客户端一起陪葬；
串行 SCAN 每页之间隔一个往返，Redis 在间隙里服务别人，占用上界是 t_scan / (t_scan + RTT)。
WAN 上占空比天然低；**本机连生产实例时 RTT 近乎为零**，全速循环会把占空比推向 100%。

所以页间睡「上一页的实测耗时」（`scan_pacing_delay`），把本机占空比压回一半左右，封顶 `SCAN_TASK_PACING_CAP`：

- 计时在锁内起。锁等待是交互命令在用连接，不是服务端为这一页付出的开销，算进去会放大等待。
- 封顶是给 WAN 的：页耗时被 RTT 主导时占空比本来就低，不该再对着 RTT 翻倍等待。
- 最后一页（游标归零）不睡，任务立刻完成。

**不用固定间隔。** 固定间隔的代价随库的规模线性放大：3356 万 key 约 1.7 万页，每页固定睡 40ms 就是 11 分钟纯睡眠，
而扫描本身不到 1 分钟。搜索是用户盯着等的前台路径，等不起按最坏假设付费的间隔；自适应等待随实际开销缩放，
总时长约为实际工作量的两倍。

**也不用「调大 COUNT」代替节流。** COUNT 越大，单条 SCAN 挡事件循环越久，与节流要保护的目标相反；
而自适应节流下总时长与 COUNT 基本无关。

## 不做

- **批量做成语义化命令**，见「任务不是命令」。
- **试算借 `search_keys` 或另一条扫描路径来数**，见「试算」。
- **批量删除报「未变更」**，见「批量删除：只报 succeeded」。
- **批量模式自动补 `*`**，见「模式原样」。
- **搜索进度显示精确的「已扫 X / 总量」。** MATCH 下拿不到已检查数，游标估算只能标「约」，见「扫描任务：只报命中数」。
- **扫描任务用独立连接 + `CLIENT KILL` 取消。** 一页 SCAN 毫秒级，丢弃在途那页的响应即可；独立连接在这个项目里意味着
  再过一遍 SSH 隧道 / TLS 的生命周期。Monitor 走 `open_dedicated_client` 是因为长连接不能有响应超时，不是先例。
  COUNT 被调得极大、单页占锁过久时才值得重新评估。
- **固定间隔节流，或用调大 COUNT 代替节流**，见「节流：按占空比自适应」。
