//! 按类型划分的语义化取值 / 写入命令。
//!
//! 这一层的存在理由见 `doc/backend/ipc-contract.md`：v0.1 里前端自己拼
//! `HSCAN` / `LRANGE` / `ZRANGE … WITHSCORES` / `XRANGE`，于是 RESP2 与 RESP3 的
//! 形状差异、三种分页语义、集群路由全都泄漏进了 Vue 组件。
//!
//! **参数形状按每种类型真实的分页语义设计，不做统一抽象**：
//!   - Hash / Set   游标（无序、可能重复、`COUNT` 是提示不是保证）
//!   - List / ZSet  下标区间（有序、精确 —— SCAN 家族给不了顺序）
//!   - Stream       ID 区间（开闭语义特殊）
//!
//! 硬塞进一个签名就会退化成一个大 union，这正是 `redis_get_key_detail`
//! 撑不起来、只能删掉的原因。

use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};

pub use super::clone_task::{get_clone_task, start_clone_task};
use crate::error::{AppError, AppResult};
use crate::models::{RedisText, TtlCondition, TtlMode};
use crate::services::capabilities::{self, CapabilityId, CapabilityState};
use crate::services::connection::{session_runtime, writable_runtime};
use crate::services::executor::{PipelineTarget, RedisExecutor};
use crate::services::redis_values::{
    optional_bytes, text_from_bytes, text_from_bytes_limited, text_from_window, DEFAULT_VALUE_LIMIT,
};
use crate::state::AppState;

pub use super::key_write::{
    clone_key, create_key, key_exists, CreateKeyValue, KeyExpiry, KeyWriteResult,
};

/// 集合成员的单条截断上限。
///
/// 比 STRING 的上限小得多：一页 200 条，每条都按 256 KB 算，
/// 一页就是 50 MB。真要看完整值就点开那一行的详情，在抽屉里重取 ——
/// 那条路径是 `get_full_value`；Set / ZSet 成员没有定位符，重取不了。
/// 见 `doc/backend/ipc-contract.md`「完整值重取」。
const MEMBER_VALUE_LIMIT: usize = 64 * 1024;

/// 按定位符重取单个完整值时的截断上限。
///
/// `codec.rs` 的 `MAX_CODEC_PAYLOAD_BYTES` 是 20 MB，但 base64 后膨胀 33%
/// → 27 MB 过 IPC 会卡。8 MB 覆盖绝大多数真实的序列化值，base64 后约 11 MB
/// 还能接受。
///
/// **它只服务显式请求**（用户点了抽屉横幅上的「加载完整值」），
/// 绝不是把 `MEMBER_VALUE_LIMIT` 直接放大 —— 每打开一个 key 就搬 8 MB 是灾难。
const FULL_VALUE_LIMIT: usize = 8 * 1024 * 1024;

/* ── 返回形状 ─────────────────────────────────────────── */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HashEntry {
    pub field: RedisText,
    pub value: RedisText,
}

/// 游标分页的一页。`completed` 是游标回到 0，不是"取够了"。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HashPage {
    pub entries: Vec<HashEntry>,
    pub cursor: String,
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberPage {
    pub members: Vec<RedisText>,
    pub cursor: String,
    pub completed: bool,
}

/// 下标分页的一页。带 `total` 是因为下标分页需要知道边界在哪 ——
/// 游标分页靠 cursor 回零判断结束，下标分页靠长度。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPage {
    pub items: Vec<RedisText>,
    /// 本页第一条在整个列表中的下标，用来显示行号。
    pub start: i64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZSetMember {
    pub member: RedisText,
    /// 分数按**字符串**回传：Redis 的分数是 double，`inf` / `-inf` 是合法值，
    /// 而 JSON 装不下它们。原样给出去，也符合"把 Redis 的真相摊给你看"。
    pub score: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZSetPage {
    pub members: Vec<ZSetMember>,
    pub start: i64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEntry {
    pub id: String,
    pub fields: Vec<HashEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamPage {
    pub entries: Vec<StreamEntry>,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamGroup {
    pub name: String,
    pub consumers: u64,
    pub pending: u64,
    pub last_delivered_id: String,
}

/* ── 参数解码 ─────────────────────────────────────────── */

/// 写入的值一律以 base64 字节传进来。
///
/// 不是为了好看：Redis 的值是任意字节，而 JSON 字符串装不下非 UTF-8 序列。
/// 用文本传参会让"把一个 gzip 值原样写回去"这件事从一开始就不可能。
fn decode_arg(base64_value: &str) -> AppResult<Vec<u8>> {
    general_purpose::STANDARD
        .decode(base64_value)
        .map_err(|error| AppError::InvalidCommand(format!("Invalid base64 argument: {error}")))
}

fn decode_args(values: &[String]) -> AppResult<Vec<Vec<u8>>> {
    values.iter().map(|value| decode_arg(value)).collect()
}

/* ── 回复解析 ─────────────────────────────────────────── */

/// SCAN 家族的回复：`[cursor, [elements…]]`。
/// cursor 在 RESP2 下是 bulk string，RESP3 下可能是整数 —— 这里一并吃掉。
fn parse_scan_reply(value: redis::Value) -> AppResult<(String, Vec<redis::Value>)> {
    let redis::Value::Array(mut parts) = value else {
        return Err(AppError::RedisCommandFailed(
            "SCAN reply is not an array".into(),
        ));
    };
    if parts.len() < 2 {
        return Err(AppError::RedisCommandFailed(
            "SCAN reply is missing the cursor or the element list".into(),
        ));
    }
    let elements = match parts.remove(1) {
        redis::Value::Array(items) => items,
        redis::Value::Set(items) => items,
        other => vec![other],
    };
    let cursor = match parts.remove(0) {
        redis::Value::BulkString(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        redis::Value::SimpleString(text) => text,
        redis::Value::Int(number) => number.to_string(),
        _ => "0".to_string(),
    };
    Ok((cursor, elements))
}

/// 展平成一维数组。RESP3 下 HGETALL / ZRANGE 可能回成对数组或 map，
/// RESP2 下是扁平的 —— 前端曾用 `Array.isArray(reply[0])` 嗅探这件事。
fn flatten_pairs(items: Vec<redis::Value>) -> Vec<redis::Value> {
    let mut out = Vec::with_capacity(items.len() * 2);
    for item in items {
        match item {
            redis::Value::Array(mut pair) if pair.len() == 2 => {
                out.push(pair.remove(0));
                out.push(pair.remove(0));
            }
            other => out.push(other),
        }
    }
    out
}

fn as_map_entries(value: redis::Value) -> Vec<redis::Value> {
    match value {
        redis::Value::Map(entries) => {
            let mut out = Vec::with_capacity(entries.len() * 2);
            for (key, value) in entries {
                out.push(key);
                out.push(value);
            }
            out
        }
        redis::Value::Array(items) => flatten_pairs(items),
        redis::Value::Set(items) => flatten_pairs(items),
        redis::Value::Nil => Vec::new(),
        other => vec![other],
    }
}

fn member_text(value: Option<&redis::Value>) -> RedisText {
    optional_bytes(value)
        .map(|bytes| text_from_bytes_limited(&bytes, MEMBER_VALUE_LIMIT))
        .unwrap_or_default()
}

fn short_text(value: Option<&redis::Value>) -> RedisText {
    optional_bytes(value)
        .map(|bytes| text_from_bytes(&bytes))
        .unwrap_or_default()
}

fn plain_string(value: Option<&redis::Value>) -> String {
    optional_bytes(value)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

fn pairs_to_entries(items: Vec<redis::Value>) -> Vec<HashEntry> {
    let mut entries = Vec::with_capacity(items.len() / 2);
    let mut index = 0;
    while index + 1 < items.len() {
        entries.push(HashEntry {
            field: short_text(items.get(index)),
            value: member_text(items.get(index + 1)),
        });
        index += 2;
    }
    entries
}

async fn count_of(executor: &mut RedisExecutor, command: &str, key: &str) -> AppResult<u64> {
    let mut size = redis::cmd(command);
    size.arg(key);
    let value = executor.query_raw(size).await?;
    Ok(crate::services::redis_values::optional_value::<u64>(Some(&value)).unwrap_or(0))
}

/* ── STRING ───────────────────────────────────────────── */

/// STRING 的窗口取值 pipeline：`STRLEN` + `GETRANGE 0 (limit-1)`，
/// 一次往返同时拿到真实总长和截断窗口。
///
/// 不用 `GET`：服务端会把**整个值**复制进这条连接的输出缓冲区再全量搬回来，
/// 只为在本地截出一个窗口 —— 512 MiB 的 key 对 256 KiB 的窗口是两千倍放大，
/// 服务端内存瞬时多出一个值的大小，传输本身还会顶穿 `REDIS_RESPONSE_TIMEOUT`
/// （百兆链路上 512 MiB 要 40 秒以上）。取值路径截的就是原始字节，
/// `GETRANGE` 与「GET 后在本地截」逐字节等价。
///
/// 单独抽出来是为了能单测：`GETRANGE` 的区间是**闭区间**，末下标写成
/// `limit` 只会安静地多搬一个字节，而单元测试能看见。
fn string_window_probe(key: &str, limit: usize) -> redis::Pipeline {
    let mut probe = redis::pipe();
    probe.cmd("STRLEN").arg(key);
    probe.cmd("GETRANGE").arg(key).arg(0).arg(limit - 1);
    probe
}

/// 发窗口 pipeline 并组装 `RedisText`。`get_string_value` 与
/// `get_full_value` 的 string 分支共用，只差 `limit`。
async fn windowed_string_value(
    executor: &mut RedisExecutor,
    key: &str,
    limit: usize,
) -> AppResult<RedisText> {
    let reply = executor
        .query_pipeline_raw(string_window_probe(key, limit), PipelineTarget::Key(key))
        .await?;
    // GETRANGE 那一格是值本身，它的错误（WRONGTYPE、ACL）要如实抛出，
    // 不能借着 `ignore_errors` 吞成空值 —— 对应旧写法里 GET 直接报错。
    // STRLEN 那一格坏了则降级：值还在，只是总长未知，读作 0 交给
    // `text_from_window` 的「窗口取满即截断」兜底。
    if let Some(redis::Value::ServerError(error)) = reply.get(1) {
        return Err(AppError::from_server_error("GETRANGE", error));
    }
    let reported_len =
        crate::services::redis_values::optional_value::<u64>(reply.first()).unwrap_or(0);
    let bytes = optional_bytes(reply.get(1)).unwrap_or_default();
    Ok(text_from_window(&bytes, reported_len, limit))
}

pub async fn get_string_value(
    state: &AppState,
    connection_id: &str,
    key: &str,
) -> AppResult<RedisText> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;
    windowed_string_value(&mut executor, key, DEFAULT_VALUE_LIMIT).await
}

/// `KEEPTTL` 不可用时的保存：`PTTL` → `SET` → 有 TTL 则 `PEXPIRE`。
///
/// 必须是脚本：分两条发会在 `SET` 与 `PEXPIRE` 之间留下一个永久 key 的窗口，
/// 同 `DELETE_LIST_ITEM_SCRIPT` 不拆成 `LSET` + `LREM` 的理由。
const SET_KEEP_TTL_SCRIPT: &str = r#"
local ttl = redis.call('PTTL', KEYS[1])
redis.call('SET', KEYS[1], ARGV[1])
if ttl > 0 then redis.call('PEXPIRE', KEYS[1], ttl) end
return 'OK'
"#;

/// 保存只更新值，原样保留原有过期时间 —— 裸 `SET` 会清掉 TTL。
///
/// 三条路径按能力 `setKeepTtl` 走，见 doc/backend/capabilities.md「STRING 保存」：
/// 支持与未知都发 `SET … KEEPTTL`（未知时被拒就降级）；不支持走脚本；
/// 脚本也不可用就拒绝保存。**绝不退回裸 `SET`。**
pub async fn set_string_value(
    state: &AppState,
    connection_id: &str,
    key: &str,
    value_base64: &str,
) -> AppResult<()> {
    let bytes = decode_arg(value_base64)?;
    let runtime = writable_runtime(state, connection_id, "SET").await?;
    let keep_ttl = runtime.capability(CapabilityId::SetKeepTtl);
    let mut executor = runtime.executor.lock().await;

    if keep_ttl.state == CapabilityState::Unsupported {
        let scripting = runtime.capability(CapabilityId::Scripting);
        if !scripting.usable {
            return Err(AppError::CapabilityUnsupported {
                message: "服务端不支持 KEEPTTL，Lua 脚本也不可用，无法在保留过期时间的前提下保存"
                    .into(),
                detail: Some(scripting.reason),
            });
        }
        let mut command = redis::cmd("EVAL");
        command.arg(SET_KEEP_TTL_SCRIPT).arg(1).arg(key).arg(bytes);
        let result = executor.query_raw(command).await;
        capabilities::guard(
            state,
            connection_id,
            &runtime,
            CapabilityId::Scripting,
            result,
        )?;
        return Ok(());
    }

    let mut command = redis::cmd("SET");
    command.arg(key).arg(bytes).arg("KEEPTTL");
    let result = executor.query_raw(command).await;
    capabilities::guard(
        state,
        connection_id,
        &runtime,
        CapabilityId::SetKeepTtl,
        result,
    )?;
    Ok(())
}

/* ── HYPERLOGLOG（string 的子形态）───────────────────── */

/// `redis_inspect_hll` 一次取回的窗口上限。dense 恒为 12,304 B；sparse 超过
/// `hll-sparse-max-bytes` 就会转 dense，正常值远小于这个数 —— 超出窗口的
/// "HLL"不是正常 HLL，`hll::inspect` 会据此降级而不是把整个大值搬过 IPC。
const HLL_INSPECT_FETCH_LIMIT: usize = 64 * 1024;

/// PFCOUNT。技术上它会回写头部的 8 字节基数缓存（物理字节变、逻辑值不变），
/// ACL 归类是 `@read`，所以走读路径、只读会话照常可用。
/// 对非 HLL 值服务端会报 WRONGTYPE —— 原样透传，前端靠它进入回退态。
pub async fn get_hll_count(state: &AppState, connection_id: &str, key: &str) -> AppResult<u64> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;
    count_of(&mut executor, "PFCOUNT", key).await
}

/// PFADD。元素是任意字节所以走 base64；返回"基数是否发生了变化"。
pub async fn add_hll_elements(
    state: &AppState,
    connection_id: &str,
    key: &str,
    elements_base64: &[String],
) -> AppResult<bool> {
    if elements_base64.is_empty() {
        return Err(AppError::InvalidCommand(
            "PFADD requires at least one element".into(),
        ));
    }
    let elements = decode_args(elements_base64)?;
    let runtime = writable_runtime(state, connection_id, "PFADD").await?;
    let mut executor = runtime.executor.lock().await;
    let mut command = redis::cmd("PFADD");
    command.arg(key);
    for element in &elements {
        command.arg(element.as_slice());
    }
    let value = executor.query_raw(command).await?;
    Ok(crate::services::redis_values::optional_value::<i64>(Some(&value)).unwrap_or(0) == 1)
}

/// 结构查看器的数据：头部字段 + 展开成 16,384 个值的寄存器数组。
/// 解析全部在 Rust 做（6-bit 解包与 sparse 操作码是协议知识），
/// 前端只负责把数组画成热力图。原始字节视图走现有的 `get_string_value`。
pub async fn inspect_hll(
    state: &AppState,
    connection_id: &str,
    key: &str,
) -> AppResult<crate::services::hll::HllInspect> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;
    let reply = executor
        .query_pipeline_raw(
            string_window_probe(key, HLL_INSPECT_FETCH_LIMIT),
            PipelineTarget::Key(key),
        )
        .await?;
    let actual_bytes =
        crate::services::redis_values::optional_value::<u64>(reply.first()).unwrap_or(0);
    let bytes = optional_bytes(reply.get(1)).unwrap_or_default();
    Ok(crate::services::hll::inspect(&bytes, actual_bytes))
}

/* ── HASH ─────────────────────────────────────────────── */

pub async fn get_hash_entries(
    state: &AppState,
    connection_id: &str,
    key: &str,
    cursor: &str,
    pattern: Option<String>,
    count: Option<usize>,
) -> AppResult<HashPage> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;

    let mut command = redis::cmd("HSCAN");
    command.arg(key).arg(cursor);
    if let Some(pattern) = pattern.filter(|value| !value.is_empty()) {
        command.arg("MATCH").arg(pattern);
    }
    command.arg("COUNT").arg(count.unwrap_or(200));

    let (next_cursor, items) = parse_scan_reply(executor.query_raw(command).await?)?;
    Ok(HashPage {
        entries: pairs_to_entries(flatten_pairs(items)),
        completed: next_cursor == "0",
        cursor: next_cursor,
    })
}

pub async fn set_hash_field(
    state: &AppState,
    connection_id: &str,
    key: &str,
    field_base64: &str,
    value_base64: &str,
) -> AppResult<()> {
    let field = decode_arg(field_base64)?;
    let value = decode_arg(value_base64)?;
    let runtime = writable_runtime(state, connection_id, "HSET").await?;
    let mut executor = runtime.executor.lock().await;
    let mut command = redis::cmd("HSET");
    command.arg(key).arg(field).arg(value);
    executor.query_raw(command).await?;
    Ok(())
}

pub async fn delete_hash_fields(
    state: &AppState,
    connection_id: &str,
    key: &str,
    fields_base64: &[String],
) -> AppResult<u64> {
    let fields = decode_args(fields_base64)?;
    if fields.is_empty() {
        return Ok(0);
    }
    let runtime = writable_runtime(state, connection_id, "HDEL").await?;
    let mut executor = runtime.executor.lock().await;
    let mut command = redis::cmd("HDEL");
    command.arg(key);
    for field in fields {
        command.arg(field);
    }
    let value = executor.query_raw(command).await?;
    Ok(crate::services::redis_values::optional_value::<u64>(Some(&value)).unwrap_or(0))
}

/* ── SET ──────────────────────────────────────────────── */

pub async fn get_set_members(
    state: &AppState,
    connection_id: &str,
    key: &str,
    cursor: &str,
    pattern: Option<String>,
    count: Option<usize>,
) -> AppResult<MemberPage> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;

    let mut command = redis::cmd("SSCAN");
    command.arg(key).arg(cursor);
    if let Some(pattern) = pattern.filter(|value| !value.is_empty()) {
        command.arg("MATCH").arg(pattern);
    }
    command.arg("COUNT").arg(count.unwrap_or(200));

    let (next_cursor, items) = parse_scan_reply(executor.query_raw(command).await?)?;
    Ok(MemberPage {
        members: items.iter().map(|item| member_text(Some(item))).collect(),
        completed: next_cursor == "0",
        cursor: next_cursor,
    })
}

pub async fn add_set_members(
    state: &AppState,
    connection_id: &str,
    key: &str,
    members_base64: &[String],
) -> AppResult<u64> {
    let members = decode_args(members_base64)?;
    if members.is_empty() {
        return Ok(0);
    }
    let runtime = writable_runtime(state, connection_id, "SADD").await?;
    let mut executor = runtime.executor.lock().await;
    let mut command = redis::cmd("SADD");
    command.arg(key);
    for member in members {
        command.arg(member);
    }
    let value = executor.query_raw(command).await?;
    Ok(crate::services::redis_values::optional_value::<u64>(Some(&value)).unwrap_or(0))
}

pub async fn delete_set_members(
    state: &AppState,
    connection_id: &str,
    key: &str,
    members_base64: &[String],
) -> AppResult<u64> {
    let members = decode_args(members_base64)?;
    if members.is_empty() {
        return Ok(0);
    }
    let runtime = writable_runtime(state, connection_id, "SREM").await?;
    let mut executor = runtime.executor.lock().await;
    let mut command = redis::cmd("SREM");
    command.arg(key);
    for member in members {
        command.arg(member);
    }
    let value = executor.query_raw(command).await?;
    Ok(crate::services::redis_values::optional_value::<u64>(Some(&value)).unwrap_or(0))
}

/* ── LIST ─────────────────────────────────────────────── */

pub async fn get_list_range(
    state: &AppState,
    connection_id: &str,
    key: &str,
    start: i64,
    stop: i64,
) -> AppResult<ListPage> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;

    let total = count_of(&mut executor, "LLEN", key).await?;
    let mut command = redis::cmd("LRANGE");
    command.arg(key).arg(start).arg(stop);
    let value = executor.query_raw(command).await?;
    let items = match value {
        redis::Value::Array(items) => items,
        redis::Value::Nil => Vec::new(),
        other => vec![other],
    };

    Ok(ListPage {
        items: items.iter().map(|item| member_text(Some(item))).collect(),
        start,
        total,
    })
}

pub async fn set_list_item(
    state: &AppState,
    connection_id: &str,
    key: &str,
    index: i64,
    value_base64: &str,
) -> AppResult<()> {
    let bytes = decode_arg(value_base64)?;
    let runtime = writable_runtime(state, connection_id, "LSET").await?;
    let mut executor = runtime.executor.lock().await;
    let mut command = redis::cmd("LSET");
    command.arg(key).arg(index).arg(bytes);
    executor.query_raw(command).await?;
    Ok(())
}

pub async fn push_list_item(
    state: &AppState,
    connection_id: &str,
    key: &str,
    value_base64: &str,
    tail: bool,
) -> AppResult<u64> {
    let bytes = decode_arg(value_base64)?;
    let intent = if tail { "RPUSH" } else { "LPUSH" };
    let runtime = writable_runtime(state, connection_id, intent).await?;
    let mut executor = runtime.executor.lock().await;
    let mut command = redis::cmd(intent);
    command.arg(key).arg(bytes);
    let value = executor.query_raw(command).await?;
    Ok(crate::services::redis_values::optional_value::<u64>(Some(&value)).unwrap_or(0))
}

/// 按下标删除一个 List 元素。
///
/// Redis 没有这个命令，惯用做法是 `LSET` 成哨兵再 `LREM` 掉。前端 v0.1 就是
/// 这么做的（`ListViewer.vue`），但那是**两条独立命令**：中间任何一次并发写
/// 都会让 LREM 删到别的元素上，而且用户永远不会知道。
///
/// 包成 Lua 就是原子的。哨兵用每次调用现生成的 UUID —— 脚本本身已经原子，
/// 这一步是为了防止列表里恰好存在一个与哨兵相同的元素被 LREM 抢先删掉。
const DELETE_LIST_ITEM_SCRIPT: &str = r#"
local current = redis.call('LINDEX', KEYS[1], ARGV[1])
if current == false then return 0 end
redis.call('LSET', KEYS[1], ARGV[1], ARGV[2])
redis.call('LREM', KEYS[1], 1, ARGV[2])
return 1
"#;

pub async fn delete_list_item(
    state: &AppState,
    connection_id: &str,
    key: &str,
    index: i64,
) -> AppResult<bool> {
    let runtime = writable_runtime(state, connection_id, "LREM").await?;
    capabilities::require(&runtime, CapabilityId::Scripting)?;
    let mut executor = runtime.executor.lock().await;
    let sentinel = format!("__tsubasa_del_{}", uuid::Uuid::new_v4());

    let mut command = redis::cmd("EVAL");
    command
        .arg(DELETE_LIST_ITEM_SCRIPT)
        .arg(1)
        .arg(key)
        .arg(index)
        .arg(sentinel);
    let result = executor.query_raw(command).await;
    let value = capabilities::guard(
        state,
        connection_id,
        &runtime,
        CapabilityId::Scripting,
        result,
    )?;
    Ok(crate::services::redis_values::optional_value::<i64>(Some(&value)).unwrap_or(0) == 1)
}

/* ── ZSET ─────────────────────────────────────────────── */

pub async fn get_zset_range(
    state: &AppState,
    connection_id: &str,
    key: &str,
    start: i64,
    stop: i64,
    rev: bool,
) -> AppResult<ZSetPage> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;

    let total = count_of(&mut executor, "ZCARD", key).await?;
    let mut command = redis::cmd(if rev { "ZREVRANGE" } else { "ZRANGE" });
    command.arg(key).arg(start).arg(stop).arg("WITHSCORES");
    let value = executor.query_raw(command).await?;

    // RESP2 是 [m, s, m, s…]，RESP3 可能是 [[m, s], …] —— 差异在这里吃掉，
    // 不再让前端用 Array.isArray(reply[0]) 去嗅探。
    let items = as_map_entries(value);
    let mut members = Vec::with_capacity(items.len() / 2);
    let mut index = 0;
    while index + 1 < items.len() {
        members.push(ZSetMember {
            member: member_text(items.get(index)),
            score: plain_string(items.get(index + 1)),
        });
        index += 2;
    }

    Ok(ZSetPage {
        members,
        start,
        total,
    })
}

pub async fn add_zset_member(
    state: &AppState,
    connection_id: &str,
    key: &str,
    member_base64: &str,
    score: &str,
) -> AppResult<()> {
    let member = decode_arg(member_base64)?;
    let runtime = writable_runtime(state, connection_id, "ZADD").await?;
    let mut executor = runtime.executor.lock().await;
    let mut command = redis::cmd("ZADD");
    command.arg(key).arg(score).arg(member);
    executor.query_raw(command).await?;
    Ok(())
}

pub async fn delete_zset_members(
    state: &AppState,
    connection_id: &str,
    key: &str,
    members_base64: &[String],
) -> AppResult<u64> {
    let members = decode_args(members_base64)?;
    if members.is_empty() {
        return Ok(0);
    }
    let runtime = writable_runtime(state, connection_id, "ZREM").await?;
    let mut executor = runtime.executor.lock().await;
    let mut command = redis::cmd("ZREM");
    command.arg(key);
    for member in members {
        command.arg(member);
    }
    let value = executor.query_raw(command).await?;
    Ok(crate::services::redis_values::optional_value::<u64>(Some(&value)).unwrap_or(0))
}

/* ── STREAM ───────────────────────────────────────────── */

pub async fn get_stream_entries(
    state: &AppState,
    connection_id: &str,
    key: &str,
    start: &str,
    end: &str,
    count: Option<usize>,
    rev: bool,
) -> AppResult<StreamPage> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;

    let total = count_of(&mut executor, "XLEN", key).await?;
    // XREVRANGE 的区间端点是反着写的：先 end 再 start。
    let mut command = redis::cmd(if rev { "XREVRANGE" } else { "XRANGE" });
    command.arg(key);
    if rev {
        command.arg(end).arg(start);
    } else {
        command.arg(start).arg(end);
    }
    if let Some(count) = count {
        command.arg("COUNT").arg(count);
    }

    let value = executor.query_raw(command).await?;
    let rows = match value {
        redis::Value::Array(items) => items,
        _ => Vec::new(),
    };

    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let redis::Value::Array(mut parts) = row else {
            continue;
        };
        if parts.is_empty() {
            continue;
        }
        let id = plain_string(Some(&parts.remove(0)));
        let fields = parts
            .into_iter()
            .next()
            .map(|value| pairs_to_entries(as_map_entries(value)))
            .unwrap_or_default();
        entries.push(StreamEntry { id, fields });
    }

    Ok(StreamPage { entries, total })
}

pub async fn get_stream_groups(
    state: &AppState,
    connection_id: &str,
    key: &str,
) -> AppResult<Vec<StreamGroup>> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;

    let mut command = redis::cmd("XINFO");
    command.arg("GROUPS").arg(key);
    let value = executor.query_raw(command).await?;
    let rows = match value {
        redis::Value::Array(items) => items,
        _ => Vec::new(),
    };

    Ok(rows
        .into_iter()
        .map(|row| {
            // 每一组是一串 field/value 对，字段顺序不保证，所以按名字取。
            let fields = as_map_entries(row);
            let mut group = StreamGroup {
                name: String::new(),
                consumers: 0,
                pending: 0,
                last_delivered_id: String::new(),
            };
            let mut index = 0;
            while index + 1 < fields.len() {
                let name = plain_string(fields.get(index));
                let value = fields.get(index + 1);
                match name.as_str() {
                    "name" => group.name = plain_string(value),
                    "consumers" => {
                        group.consumers =
                            crate::services::redis_values::optional_value::<u64>(value).unwrap_or(0)
                    }
                    "pending" => {
                        group.pending =
                            crate::services::redis_values::optional_value::<u64>(value).unwrap_or(0)
                    }
                    "last-delivered-id" => group.last_delivered_id = plain_string(value),
                    _ => {}
                }
                index += 2;
            }
            group
        })
        .collect())
}

/* ── 按定位符重取完整值 ───────────────────────────────── */

/// 一个值在库里的定位符。
///
/// **这不是 `redis_get_key_detail` 的覆辙。** 那个撑不住是因为它把三种不同的
/// **分页语义**（游标 / 下标 / ID 区间）塞进一个签名：参数互斥、返回形状随类型变。
/// 这里是**同一个语义**（按定位符取单个完整值）的三种定位形态，返回形状恒定是
/// `RedisText`，是真正的 sum type。
///
/// **没有 Set / ZSet 变体，而且补不出来**：集合成员本身既是值又是定位符，
/// 要精确定位它得先有完整成员，而你正是因为它被截断了才要重取。这是 Redis 的
/// 数据模型而不是实现偷懒，界面上要如实说出这个原因。
/// 两条都见 `doc/backend/ipc-contract.md`「完整值重取」。
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ValueRef {
    String {
        key: String,
    },
    HashField {
        key: String,
        /// field 可以是任意字节，所以跟写入参数一样走 base64。
        field_base64: String,
    },
    ListIndex {
        key: String,
        index: i64,
    },
}

/// 成员定位符 → 一条只读命令。
///
/// 单独抽出来是为了能单测：命令名或参数顺序错了，`get_full_value` 只会
/// 安静地回一个空值，而单元测试能看见。
fn full_value_command(target: &ValueRef) -> AppResult<redis::Cmd> {
    match target {
        // string 不在这里：整值 GET 对大 key 是服务端内存放大 + 必超时，
        // `get_full_value` 把它分流进 `windowed_string_value` 的窗口路径。
        // 这一臂只防直接调用 —— 谁想加回 GET，先看 `string_window_probe`。
        ValueRef::String { .. } => Err(AppError::InvalidCommand(
            "string locators take the windowed pipeline path".into(),
        )),
        ValueRef::HashField { key, field_base64 } => {
            // 必须按原始字节发：拿界面上显示的那串十六进制去 HGET，取回来的永远是 nil
            let field = decode_arg(field_base64)?;
            let mut command = redis::cmd("HGET");
            command.arg(key).arg(field);
            Ok(command)
        }
        ValueRef::ListIndex { key, index } => {
            let mut command = redis::cmd("LINDEX");
            command.arg(key).arg(*index);
            Ok(command)
        }
    }
}

/// 按定位符取回单个值，上限 `FULL_VALUE_LIMIT`。
///
/// `total_bytes` 的契约不动：重取成功 → `None` → 允许保存；仍然超限 → 非空 →
/// 仍然只读。与 `doc/backend/ipc-contract.md`「截断契约」自洽。
pub async fn get_full_value(
    state: &AppState,
    connection_id: &str,
    target: &ValueRef,
) -> AppResult<RedisText> {
    // string 走与 get_string_value 同一条窗口路径，只是上限放大到
    // FULL_VALUE_LIMIT；成员值没有范围读命令，仍是取回后截断
    if let ValueRef::String { key } = target {
        let runtime = session_runtime(state, connection_id).await?;
        let mut executor = runtime.executor.lock().await;
        return windowed_string_value(&mut executor, key, FULL_VALUE_LIMIT).await;
    }
    // 命令先拼好：base64 是坏的就该在借到连接之前失败
    let command = full_value_command(target)?;
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;
    let value = executor.query_raw(command).await?;
    Ok(optional_bytes(Some(&value))
        .map(|bytes| text_from_bytes_limited(&bytes, FULL_VALUE_LIMIT))
        .unwrap_or_default())
}

/* ── 会话级信息 ───────────────────────────────────────── */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseInfo {
    pub index: i64,
    /// `INFO keyspace` 里见到的 key 数；该库为空时不出现，此处为 `None`。
    pub keys: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerMemory {
    pub used: Option<String>,
    /// `maxmemory` 为 0 表示不限制，这时给 `None` 而不是 "0B"。
    pub max: Option<String>,
}

fn parse_info_field<'a>(info: &'a str, field: &str) -> Option<&'a str> {
    info.lines()
        .find_map(|line| line.trim().strip_prefix(field)?.strip_prefix(':'))
        .map(str::trim)
}

/// DB 列表。
///
/// `CONFIG GET databases` 是正解；它拿不到时（旧版只读守卫、ACL、
/// `rename-command`）退回 `INFO keyspace` 见到的最大下标，再兜底 16。
/// 这个回退链 v0.1 在前端 —— 它需要知道"CONFIG 可能被拒"这件后端的事。
pub async fn get_database_list(
    state: &AppState,
    connection_id: &str,
) -> AppResult<Vec<DatabaseInfo>> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;

    // 集群只有 db0，SELECT 在集群下本来就不可用。
    if executor.is_cluster() {
        return Ok(vec![DatabaseInfo {
            index: 0,
            keys: None,
        }]);
    }

    let mut keyspace = redis::cmd("INFO");
    keyspace.arg("keyspace");
    let info = executor
        .query_raw(keyspace)
        .await
        .ok()
        .and_then(|value| optional_bytes(Some(&value)))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();

    let mut sizes = std::collections::BTreeMap::new();
    for line in info.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("db") else {
            continue;
        };
        let Some((index, stats)) = rest.split_once(':') else {
            continue;
        };
        let Ok(index) = index.parse::<i64>() else {
            continue;
        };
        let keys = stats
            .split(',')
            .find_map(|part| part.trim().strip_prefix("keys="))
            .and_then(|value| value.parse::<u64>().ok());
        if let Some(keys) = keys {
            sizes.insert(index, keys);
        }
    }

    let mut config = redis::cmd("CONFIG");
    config.arg("GET").arg("databases");
    let configured = executor
        .query_raw(config)
        .await
        .ok()
        .map(as_map_entries)
        .and_then(|entries| optional_bytes(entries.get(1)))
        .and_then(|bytes| String::from_utf8_lossy(&bytes).parse::<i64>().ok())
        .filter(|total| *total > 0);

    let total = configured.unwrap_or_else(|| {
        let max_seen = sizes.keys().copied().max().unwrap_or(0);
        (max_seen + 1).max(16)
    });

    Ok((0..total)
        .map(|index| DatabaseInfo {
            index,
            keys: sizes.get(&index).copied(),
        })
        .collect())
}

pub async fn get_server_memory(state: &AppState, connection_id: &str) -> AppResult<ServerMemory> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;

    let mut command = redis::cmd("INFO");
    command.arg("memory");
    let info = optional_bytes(Some(&executor.query_raw(command).await?))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();

    Ok(ServerMemory {
        used: parse_info_field(&info, "used_memory_human").map(str::to_string),
        max: parse_info_field(&info, "maxmemory_human")
            .filter(|value| *value != "0B")
            .map(str::to_string),
    })
}

/* ── key 级写操作 ─────────────────────────────────────── */

/// 三种模式各自对应的命令名。`TtlMode` 的定义在 `models.rs` —— 它是 IPC 契约类型。
pub fn ttl_command_name(mode: TtlMode) -> &'static str {
    match mode {
        TtlMode::Persist => "PERSIST",
        TtlMode::Relative => "EXPIRE",
        TtlMode::Absolute => "EXPIREAT",
    }
}

/// 拼一条 TTL 命令。
///
/// 单 key（`set_key_ttl`）与批量（`task::run_batch_ttl_task`）共用这一份 ——
/// 两边各拼一份的话，条件 flag 的位置、缺值的报错、`PERSIST` 不带参数这些
/// 细节迟早会漂开。
pub fn build_ttl_command(
    mode: TtlMode,
    key: &str,
    value: Option<i64>,
    condition: Option<TtlCondition>,
) -> AppResult<redis::Cmd> {
    let mut command = redis::cmd(ttl_command_name(mode));
    command.arg(key);

    if matches!(mode, TtlMode::Persist) {
        // PERSIST 没有条件 flag。前端选「不过期」时条件区整块禁用，
        // 这里是同一条规则的后端一侧。
        if condition.is_some() {
            return Err(AppError::InvalidCommand(
                "PERSIST does not take a condition".into(),
            ));
        }
        return Ok(command);
    }

    let value = value
        .ok_or_else(|| AppError::InvalidCommand("A TTL value is required for this mode".into()))?;
    command.arg(value);

    if let Some(condition) = condition {
        command.arg(match condition {
            TtlCondition::Nx => "NX",
            TtlCondition::Xx => "XX",
            TtlCondition::Gt => "GT",
            TtlCondition::Lt => "LT",
        });
    }
    Ok(command)
}

pub async fn set_key_ttl(
    state: &AppState,
    connection_id: &str,
    key: &str,
    mode: TtlMode,
    value: Option<i64>,
) -> AppResult<()> {
    let command = build_ttl_command(mode, key, value, None)?;
    let runtime = writable_runtime(state, connection_id, ttl_command_name(mode)).await?;
    let mut executor = runtime.executor.lock().await;
    executor.query_raw(command).await?;
    Ok(())
}

pub async fn rename_key(
    state: &AppState,
    connection_id: &str,
    key: &str,
    new_key: &str,
) -> AppResult<()> {
    if new_key.is_empty() {
        return Err(AppError::InvalidCommand(
            "The new key name must not be empty".into(),
        ));
    }
    let runtime = writable_runtime(state, connection_id, "RENAME").await?;
    let mut executor = runtime.executor.lock().await;
    let mut command = redis::cmd("RENAME");
    command.arg(key).arg(new_key);
    executor.query_raw(command).await?;
    Ok(())
}

pub async fn delete_keys(state: &AppState, connection_id: &str, keys: &[String]) -> AppResult<u64> {
    if keys.is_empty() {
        return Ok(0);
    }
    let runtime = writable_runtime(state, connection_id, "DEL").await?;
    let mut executor = runtime.executor.lock().await;

    // 集群下多个 key 可能跨槽位，DEL 不能一条打包发；逐条发是唯一安全的做法。
    if executor.is_cluster() && keys.len() > 1 {
        let mut deleted = 0;
        for key in keys {
            let mut command = redis::cmd("DEL");
            command.arg(key);
            let value = executor.query_raw(command).await?;
            deleted +=
                crate::services::redis_values::optional_value::<u64>(Some(&value)).unwrap_or(0);
        }
        return Ok(deleted);
    }

    let mut command = redis::cmd("DEL");
    for key in keys {
        command.arg(key);
    }
    let value = executor.query_raw(command).await?;
    Ok(crate::services::redis_values::optional_value::<u64>(Some(&value)).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resp2_scan_reply() {
        let value = redis::Value::Array(vec![
            redis::Value::BulkString(b"17".to_vec()),
            redis::Value::Array(vec![
                redis::Value::BulkString(b"field".to_vec()),
                redis::Value::BulkString(b"value".to_vec()),
            ]),
        ]);
        let (cursor, items) = parse_scan_reply(value).expect("scan reply should parse");
        assert_eq!(cursor, "17");
        assert_eq!(items.len(), 2);
    }

    /// RESP3 下游标是整数而不是 bulk string。
    #[test]
    fn parses_resp3_integer_cursor() {
        let value = redis::Value::Array(vec![
            redis::Value::Int(42),
            redis::Value::Array(vec![redis::Value::BulkString(b"a".to_vec())]),
        ]);
        let (cursor, _) = parse_scan_reply(value).expect("scan reply should parse");
        assert_eq!(cursor, "42");
    }

    /// 这正是前端曾经用 `Array.isArray(reply[0])` 嗅探的那件事。
    #[test]
    fn flattens_resp3_pair_arrays_like_resp2_flat_arrays() {
        let paired = redis::Value::Array(vec![redis::Value::Array(vec![
            redis::Value::BulkString(b"member".to_vec()),
            redis::Value::BulkString(b"1.5".to_vec()),
        ])]);
        let flat = redis::Value::Array(vec![
            redis::Value::BulkString(b"member".to_vec()),
            redis::Value::BulkString(b"1.5".to_vec()),
        ]);
        assert_eq!(as_map_entries(paired).len(), as_map_entries(flat).len());
    }

    #[test]
    fn reads_resp3_map_replies() {
        let value = redis::Value::Map(vec![(
            redis::Value::BulkString(b"field".to_vec()),
            redis::Value::BulkString(b"value".to_vec()),
        )]);
        let entries = pairs_to_entries(as_map_entries(value));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].field.text.as_deref(), Some("field"));
        assert_eq!(entries[0].value.text.as_deref(), Some("value"));
    }

    /// 非 UTF-8 的成员不能在解析这一步丢掉 —— 它们正是 codec 要处理的对象。
    #[test]
    fn keeps_non_utf8_members_as_bytes() {
        let items = [redis::Value::BulkString(vec![0x1f, 0x8b, 0x08])];
        let text = member_text(items.first());
        assert_eq!(text.text, None);
        assert!(text.base64.is_some());
    }

    #[test]
    fn reads_info_fields_by_name() {
        let info = "# Memory\r\nused_memory_human:1.25M\r\nmaxmemory_human:0B\r\n";
        assert_eq!(parse_info_field(info, "used_memory_human"), Some("1.25M"));
        assert_eq!(parse_info_field(info, "maxmemory_human"), Some("0B"));
        assert_eq!(parse_info_field(info, "missing_field"), None);
    }

    /// `used_memory` 是 `used_memory_human` 的前缀，按前缀匹配会取错行。
    #[test]
    fn info_field_lookup_does_not_match_a_longer_field_name() {
        let info = "used_memory_human:1.25M\r\nused_memory:1310720\r\n";
        assert_eq!(parse_info_field(info, "used_memory"), Some("1310720"));
    }

    #[test]
    fn rejects_invalid_base64_arguments() {
        assert!(decode_arg("not base64!!").is_err());
    }

    #[test]
    fn decodes_base64_arguments_to_raw_bytes() {
        assert_eq!(
            decode_arg("H4sI").expect("should decode"),
            vec![0x1f, 0x8b, 0x08]
        );
    }

    /* ── 按定位符重取完整值 ───────────────────────────── */

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn each_member_locator_maps_to_its_own_read_command() {
        for (target, name) in [
            (
                ValueRef::HashField {
                    key: "user:1".into(),
                    field_base64: "YQ==".into(),
                },
                "HGET",
            ),
            (
                ValueRef::ListIndex {
                    key: "user:1".into(),
                    index: 3,
                },
                "LINDEX",
            ),
        ] {
            let command = full_value_command(&target).expect("command should build");
            assert!(
                packed(&command).contains(name),
                "{name} missing from {:?}",
                packed(&command)
            );
        }
    }

    /// string 定位符不许退回整值 GET —— 那是服务端内存放大 + 必超时，
    /// 它只能走 `windowed_string_value` 的窗口路径。
    #[test]
    fn a_string_locator_never_becomes_a_full_get() {
        let target = ValueRef::String {
            key: "user:1".into(),
        };
        assert!(full_value_command(&target).is_err());
    }

    /// GETRANGE 的区间是闭区间：limit 1024 的末下标是 1023，
    /// 写成 1024 会安静地多搬一个字节。
    #[test]
    fn the_window_probe_asks_for_the_length_and_a_closed_range() {
        let packed =
            String::from_utf8_lossy(&string_window_probe("user:1", 1024).get_packed_pipeline())
                .to_string();
        assert!(packed.contains("STRLEN"));
        assert!(packed.contains("GETRANGE"));
        assert!(packed.contains("1023"));
        assert!(!packed.contains("1024"));
    }

    /// field 是任意字节。拿界面上显示的那串十六进制去 `HGET` 永远取不到东西 ——
    /// 跟抽屉必须收 `RedisText` 而不是 `textOf()` 后的字符串是同一个道理，
    /// 这里是它在后端一侧的对应物。
    #[test]
    fn hash_field_locator_sends_the_raw_bytes_not_their_hex_rendering() {
        let target = ValueRef::HashField {
            key: "cache:user:1".into(),
            // 1f 8b 08 —— gzip 的魔数，界面上显示成 "1f 8b 08"
            field_base64: "H4sI".into(),
        };
        let command = full_value_command(&target).expect("command should build");
        let packed = command.get_packed_command();

        assert!(contains_bytes(&packed, &[0x1f, 0x8b, 0x08]));
        assert!(!contains_bytes(&packed, b"1f 8b 08"));
    }

    #[test]
    fn a_broken_field_locator_fails_before_any_connection_is_borrowed() {
        let target = ValueRef::HashField {
            key: "user:1".into(),
            field_base64: "not base64!!".into(),
        };
        assert!(full_value_command(&target).is_err());
    }

    /// `src/types/ipc.ts` 里的 `ValueRef` 是手抄的镜像，没有生成器 ——
    /// 线上的字段名抄错了只有这里能发现。
    #[test]
    fn value_ref_deserializes_the_camel_case_wire_shape() {
        let target: ValueRef =
            serde_json::from_str(r#"{"kind":"hashField","key":"k","fieldBase64":"YQ=="}"#)
                .expect("hashField should deserialize");
        assert!(matches!(
            target,
            ValueRef::HashField { field_base64, .. } if field_base64 == "YQ=="
        ));

        let target: ValueRef = serde_json::from_str(r#"{"kind":"listIndex","key":"k","index":-1}"#)
            .expect("listIndex should deserialize");
        assert!(matches!(target, ValueRef::ListIndex { index: -1, .. }));

        let target: ValueRef =
            serde_json::from_str(r#"{"kind":"string","key":"k"}"#).expect("string should parse");
        assert!(matches!(target, ValueRef::String { .. }));
    }

    /// Set / ZSet 没有变体，而且补不出来：成员既是值又是定位符。
    /// 谁想"补齐"这一格，先看 `doc/backend/ipc-contract.md`「完整值重取」。
    #[test]
    fn there_is_no_locator_for_a_collection_member() {
        assert!(serde_json::from_str::<ValueRef>(r#"{"kind":"setMember","key":"k"}"#).is_err());
    }

    /// 重取要真的能拿回分页截掉的那一段，否则这条路径没有意义。
    ///
    /// `total_bytes` 的契约同时也在这里：重取成功 → `None` → 界面允许保存。
    #[test]
    fn the_refetch_limit_covers_what_the_page_limit_truncates() {
        let value = vec![b'x'; MEMBER_VALUE_LIMIT + 1];
        assert_eq!(
            text_from_bytes_limited(&value, MEMBER_VALUE_LIMIT).total_bytes,
            Some(value.len() as u64)
        );
        assert_eq!(
            text_from_bytes_limited(&value, FULL_VALUE_LIMIT).total_bytes,
            None
        );
    }

    /* ── TTL 命令 ─────────────────────────────────────── */

    fn packed(command: &redis::Cmd) -> String {
        String::from_utf8_lossy(&command.get_packed_command()).to_string()
    }

    #[test]
    fn each_ttl_mode_maps_to_its_own_command() {
        assert!(
            packed(&build_ttl_command(TtlMode::Persist, "user:1", None, None).unwrap())
                .contains("PERSIST")
        );
        assert!(packed(
            &build_ttl_command(TtlMode::Relative, "user:1", Some(86400), None).unwrap()
        )
        .contains("EXPIRE"));
        assert!(packed(
            &build_ttl_command(TtlMode::Absolute, "user:1", Some(1_700_000_000), None).unwrap()
        )
        .contains("EXPIREAT"));
    }

    #[test]
    fn persist_takes_no_value_and_relative_modes_require_one() {
        // PERSIST 只有 key，多带一个数字会变成一条不同的命令
        let persist =
            packed(&build_ttl_command(TtlMode::Persist, "user:1", Some(60), None).unwrap());
        assert!(!persist.contains("60"));

        assert!(build_ttl_command(TtlMode::Relative, "user:1", None, None).is_err());
        assert!(build_ttl_command(TtlMode::Absolute, "user:1", None, None).is_err());
    }

    #[test]
    fn conditions_go_after_the_value_and_persist_refuses_them() {
        let command = build_ttl_command(
            TtlMode::Relative,
            "user:1",
            Some(86400),
            Some(TtlCondition::Nx),
        )
        .unwrap();
        let text = packed(&command);
        let expire = text.find("EXPIRE").expect("EXPIRE");
        let value = text.find("86400").expect("86400");
        let flag = text.find("NX").expect("NX");
        assert!(
            expire < value && value < flag,
            "wrong argument order: {text:?}"
        );

        for (condition, token) in [
            (TtlCondition::Xx, "XX"),
            (TtlCondition::Gt, "GT"),
            (TtlCondition::Lt, "LT"),
        ] {
            let command = build_ttl_command(
                TtlMode::Absolute,
                "user:1",
                Some(1_700_000_000),
                Some(condition),
            )
            .unwrap();
            assert!(packed(&command).contains(token));
        }

        // PERSIST 没有条件 flag —— 界面上选「不过期」时条件区整块禁用，
        // 这里是同一条规则的后端一侧
        assert!(
            build_ttl_command(TtlMode::Persist, "user:1", None, Some(TtlCondition::Nx)).is_err()
        );
    }
}
