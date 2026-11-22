# 集群

覆盖：`src-tauri/src/services/executor.rs`（`PipelineTarget`）、`src-tauri/src/services/task.rs`（`KeyBatch`、`ScanPage`、`scan_one_page_with_nodes`、`scan_cluster_one_page`、`delete_key_chunk`、`apply_ttl_batch`、分块常量）、`src-tauri/src/services/connection.rs`（`annotate_keys`、`encode_cluster_scan_cursor`、`decode_cluster_scan_cursor`、`scan_cluster_keys`）

## Pipeline 必须说明落点

集群下 redis-rs 只按第一条命令路由**整个** pipeline，跨槽位的一批会先被打到错误的节点。
那个节点对每条不归它的命令回 MOVED，redis-rs 再把它们逐条转发到持有槽位的节点——
**结果是对的，代价是每条命令多一跳往返**，按节点整批发 pipeline 的收益全部抵消。
跨 WAN 的集群上，一批一千条就是一千次额外往返。
所以 `executor::query_pipeline_raw` 要求调用方给出 `PipelineTarget`：

- `Key`：整批命令都作用于同一个 key，按它的槽位路由。
- `Node`：整批命令都落在同一个主节点持有的槽位上，按地址路由。
- `AnyNode`：整批命令都不带 key、问的是服务端本身，任一主节点都能答。只有建连时的能力探测用它，
  见 [capabilities](capabilities.md)「探测」。

单机下这个参数被忽略。把危险的情况做成**不可表达的**，比在文档里写一句「注意不要跨槽位」可靠。

## 集群游标

集群下的一页 SCAN 是逐个主节点各扫一页。多个节点的游标编码成**单个不透明字符串**
（`cluster:` 前缀加「端点 → 游标」的 JSON，`encode_cluster_scan_cursor` / `decode_cluster_scan_cursor`），
只保留没归零的节点；全部归零就编码成 `"0"`，即「扫完」。所以调用方拿到的游标对单机与集群同形：
前端的翻页、扫描任务的循环、「扫完」的判断都不用知道是不是集群。扫描任务的进度估算也靠这一点——
合成游标里缺席的节点就是已扫完的节点，见 [tasks](tasks.md)「扫描任务：只报命中数」。

游标编解码只有一处实现（`connection.rs`）。扫描任务这一侧的扫描也只有一处：`scan_one_page_with_nodes` 保留落点，
`scan_one_page`（拍平）与 `scan_and_annotate_one_page`（补类型）都是它的视图，免得游标编解码出现第二份。
浏览路径 `redis_scan_keys` 的 `scan_cluster_keys` 另有一份逐节点循环，与任务侧共用的是游标编解码与 `annotate_keys`——
**改集群扫描时两处都要看。**

## 批量写：按节点整批或逐 key

一条通用约束，不只对批量删除与批量 TTL：

> **不能拿一个跨槽位的 chunk 去 pipeline。** 要么按节点分组，要么逐 key。

两种作用范围天然落在这两条路上：

- **模式路径按节点整批。** `scan_one_page_with_nodes` 在集群下逐个主节点 SCAN，把每页 key 连同
  「来自哪个节点」一起返回（`ScanPage` 的每个 `KeyBatch` 带 `node`）。这一批的 key 全部落在该节点持有的
  槽位上，整批 pipeline 打给它是安全的。**落点必须一路留住**——拍平成一串 key 就丢了，每个 chunk
  只能退化成逐 key 一次往返，树右键一个百万级前缀时在跨 WAN 的集群上不可用。SCAN 回 key 时补
  `TYPE` / `TTL` 的 `annotate_keys` 也是这个模式。
- **显式列表逐 key。** 用户勾选来的 key 跨槽位，没有统一落点（`node` 为 `None`），只能逐 key 串行。
  这可以接受：勾选的量级是几十到几千，不是百万。按槽位分组要引入 slot→node 映射表，这个复杂度不付。

于是每批命令的形状由「集群与否 × 有无落点」决定：

| | 批量删除（`delete_key_chunk`） | 批量 TTL（`apply_ttl_batch`） |
|---|---|---|
| 单机 | 一条多 key `DEL` | pipeline 一批单 key 命令 |
| 集群，有落点 | pipeline 一批单 key `DEL`，打给该节点 | pipeline 一批单 key 命令，打给该节点 |
| 集群，无落点 | 逐 key | 逐 key |

**没有 `MEXPIRE`。** 批量删除在单机上能 `DEL k1 k2 k3` 一条搞定，TTL 只能一 key 一条命令，
所以单机也必须走 pipeline。

## 分块大小

分块常量按**形状**分，不按功能分：

- `BATCH_DELETE_CHUNK`：一条多 key `DEL` 的参数个数。单机的删除走它（集群下逐 key 的显式列表也按它切，
  那时块大小只影响进度上报与取消检查点的粒度）。
- `BATCH_DELETE_CLUSTER_CHUNK`、`BATCH_TTL_CHUNK`：一个 pipeline 里**独立命令**的条数。
  同样多的 key，独立命令的请求体积与服务端解析开销比「一条命令带这么多参数」高一个量级，
  回包却只是同样多个 integer，所以这两个比前者小。

具体值以这三个常量为准。选哪一个由 `batch_delete_chunk_size` 按有无落点决定。

## 多 key 命令要求同槽位

按节点分组之后，发出去的仍然是「pipeline of 单 key `DEL`」，**不是**「一条多 key `DEL`」。
多 key 命令要求**同槽位**，不只是同节点；一个主节点持有一段槽位，一页扫回来的 key 几乎必然散在其中好几个上：

```
sameNode:1（槽 5419）与 sameNode:4（槽 1422）同属 :7000 这个节点
DEL sameNode:1 sameNode:4  打给 :7000  → CROSSSLOT Keys in request don't hash to the same slot
```

这一步很容易被「优化」掉，因为**单机测试全绿**——单机那条路本来就该是一条多 key `DEL`。所以它同时钉在
`delete_key_chunk` 的注释与 `tests/redis_phase5b.rs` 的 `cluster_pattern_delete_removes_every_key_it_scanned` 里。
CROSSSLOT 是真的报错，MOVED 不是——这正是两件事的区别。

## 路由的验证看服务端

因为路由错了结果照样正确，「删完之后 key 不在了」「TTL 设上了」都证明不了路由对，路由全错也会通过。
能证明的是服务端的计数：每条打错节点的命令都会在那个节点的 `INFO commandstats` 里记一次 `rejected_calls`
（Redis 7.0+）。`tests/redis_phase5b.rs` 的 `rejected_del_calls` 汇总各主节点的这个数：

- `cluster_pattern_delete_removes_every_key_it_scanned` 断言按落点删除前后它**不变**；
- `cluster_pipeline_sent_to_the_wrong_node_is_redirected_not_dropped` 故意打错节点，断言它**正好增加 key 数**
  且 key 全部删掉——钉住的是 redis-rs 跟随重定向这个行为。哪天它不再跟随，这条会红，路由错就成了正确性问题。

批量 TTL 的集群用例：`tests/redis_phase5a.rs` 的 `cluster_batch_ttl_routes_each_page_to_the_node_it_came_from`
与 `cluster_batch_ttl_falls_back_to_one_key_at_a_time_without_a_node`。

## 不做

- **把按节点分好的一批合成一条多 key 命令**，见「多 key 命令要求同槽位」。
- **给显式列表按槽位分组**（slot→node 映射表），见「批量写：按节点整批或逐 key」。
