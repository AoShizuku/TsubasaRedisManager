//! 新建与克隆的服务端原子写路径；由 values 暴露语义化入口。
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::error::{AppError, AppResult};
use crate::models::ConnectionConfig;
use crate::services::capabilities::{self, CapabilityId};
use crate::services::connection::{session_runtime, writable_runtime};
use crate::state::AppState;

const MAX_INITIAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_ROWS: usize = 256;
pub(crate) const MAX_DUMP_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum KeyExpiry {
    Persist,
    Preserve,
    Relative { milliseconds: i64 },
    Absolute { milliseconds: i64 },
}

impl KeyExpiry {
    pub(crate) fn args(&self, cloning: bool) -> AppResult<(&'static str, i64)> {
        match self {
            Self::Persist => Ok(("persist", 0)),
            Self::Preserve if cloning => Ok(("preserve", 0)),
            Self::Relative { milliseconds } | Self::Absolute { milliseconds }
                if *milliseconds > 0 && *milliseconds <= 4_000_000_000_000_000 =>
            {
                Ok((
                    if matches!(self, Self::Relative { .. }) {
                        "relative"
                    } else {
                        "absolute"
                    },
                    *milliseconds,
                ))
            }
            _ => Err(fault("invalid_expiry", "过期时间无效")),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialField {
    pub field_base64: String,
    pub value_base64: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialScoredMember {
    pub value_base64: String,
    pub score: String,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CreateKeyValue {
    String { value_base64: String },
    Hash { fields: Vec<InitialField> },
    List { elements_base64: Vec<String> },
    Set { members_base64: Vec<String> },
    Zset { members: Vec<InitialScoredMember> },
    Stream { fields: Vec<InitialField> },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyWriteResult {
    pub key_type: String,
}

pub(crate) fn fault(code: &str, message: &str) -> AppError {
    AppError::KeyWriteFailed {
        code: format!("redis.key_{code}"),
        message: message.into(),
    }
}

/// 写入结果核对使用严格的 EXISTS；不能把元信息接口的可选字段降级当成“不存在”。
pub async fn key_exists(state: &AppState, connection_id: &str, key: &str) -> AppResult<bool> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut command = redis::cmd("EXISTS");
    command.arg(key);
    let reply = runtime.executor.lock().await.query_raw(command).await?;
    redis::from_redis_value(reply).map_err(|err| AppError::RedisCommandFailed(err.to_string()))
}

pub(crate) fn map_error(error: AppError) -> AppError {
    let AppError::RedisCommandFailed(ref detail) = error else {
        return error;
    };
    for (marker, code, message) in [
        ("KEY_EXISTS", "exists", "目标 Key 已存在，请使用其他名称"),
        ("BUSYKEY", "exists", "目标 Key 已存在，请使用其他名称"),
        ("SOURCE_MISSING", "source_missing", "源 Key 已不存在"),
        (
            "SOURCE_EXPIRED",
            "source_expired",
            "源 Key 的过期时间已到，未创建副本",
        ),
        (
            "INVALID_EXPIRY",
            "invalid_expiry",
            "过期时间必须晚于服务器当前时间",
        ),
        (
            "KEY_TOO_LARGE",
            "too_large",
            "单 Key 克隆上限为 64 MiB，请使用导出流程处理更大的 Key",
        ),
    ] {
        if detail.contains(marker) {
            return fault(code, message);
        }
    }
    error
}

fn initial_args(value: CreateKeyValue) -> AppResult<(&'static str, &'static str, Vec<Vec<u8>>)> {
    let stream = matches!(value, CreateKeyValue::Stream { .. });
    let mut total = 0;
    let mut decode = |encoded: &str| -> AppResult<Vec<u8>> {
        if encoded.len() > MAX_INITIAL_BYTES.div_ceil(3) * 4 {
            return Err(fault("too_large", "初始内容上限为 8 MiB"));
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| fault("invalid_value", "Base64 内容无效"))?;
        total += bytes.len();
        if total > MAX_INITIAL_BYTES {
            return Err(fault("too_large", "初始内容上限为 8 MiB"));
        }
        Ok(bytes)
    };
    let check_rows = |len: usize| -> AppResult<()> {
        if !(1..=MAX_ROWS).contains(&len) {
            return Err(fault("invalid_value", "初始内容需要 1 至 256 行"));
        }
        Ok(())
    };
    let mut args = Vec::new();
    let (kind, command) = match value {
        CreateKeyValue::String { value_base64 } => {
            args.push(decode(&value_base64)?);
            ("string", "SET")
        }
        CreateKeyValue::Hash { fields } | CreateKeyValue::Stream { fields } => {
            check_rows(fields.len())?;
            let mut names = HashSet::new();
            if stream {
                args.push(b"*".to_vec());
            }
            for field in fields {
                let name = decode(&field.field_base64)?;
                if !stream && !names.insert(name.clone()) {
                    return Err(fault("invalid_value", "初始字段重复"));
                }
                args.push(name);
                args.push(decode(&field.value_base64)?);
            }
            if stream {
                ("stream", "XADD")
            } else {
                ("hash", "HSET")
            }
        }
        CreateKeyValue::List { elements_base64 } => {
            check_rows(elements_base64.len())?;
            for member in elements_base64 {
                args.push(decode(&member)?);
            }
            ("list", "RPUSH")
        }
        CreateKeyValue::Set { members_base64 } => {
            check_rows(members_base64.len())?;
            let mut unique = HashSet::new();
            for member in members_base64 {
                let bytes = decode(&member)?;
                if !unique.insert(bytes.clone()) {
                    return Err(fault("invalid_value", "初始成员重复"));
                }
                args.push(bytes);
            }
            ("set", "SADD")
        }
        CreateKeyValue::Zset { members } => {
            check_rows(members.len())?;
            let mut unique = HashSet::new();
            for member in members {
                let score = member
                    .score
                    .parse::<f64>()
                    .map_err(|_| fault("invalid_value", "score 必须是数字"))?;
                if score.is_nan() {
                    return Err(fault("invalid_value", "score 不能是 NaN"));
                }
                args.push(score.to_string().into_bytes());
                let bytes = decode(&member.value_base64)?;
                if !unique.insert(bytes.clone()) {
                    return Err(fault("invalid_value", "初始成员重复"));
                }
                args.push(bytes);
            }
            ("zset", "ZADD")
        }
    };
    Ok((kind, command, args))
}

// 所有初始内容只发一条写命令，避免循环中某条失败留下半个集合。
// TTL 先在已确认不存在的 key 上执行一次：无副作用，但验证命令、参数与 ACL。
// 同一 Lua 执行中 ACL 不会变化，随后创建 + TTL 不会因权限不足留下永久 key。
const CREATE_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) ~= 0 then return redis.error_reply('KEY_EXISTS') end
local expiry = 0
if ARGV[2] ~= 'persist' then
    local time = redis.call('TIME')
    local now = time[1] * 1000 + math.floor(time[2] / 1000)
    expiry = tonumber(ARGV[3])
    if ARGV[2] == 'relative' then expiry = now + expiry end
    if expiry <= now then return redis.error_reply('INVALID_EXPIRY') end
    expiry = string.format('%.0f', expiry)
    redis.call('PEXPIREAT', KEYS[1], expiry)
end
redis.call(ARGV[1], KEYS[1], unpack(ARGV, 4))
if expiry ~= 0 then redis.call('PEXPIREAT', KEYS[1], expiry) end
return 'OK'
"#;

pub async fn create_key(
    state: &AppState,
    connection_id: &str,
    key: &str,
    value: CreateKeyValue,
    expiry: KeyExpiry,
) -> AppResult<KeyWriteResult> {
    let (mode, milliseconds) = expiry.args(false)?;
    let (kind, write_command, args) = initial_args(value)?;
    let runtime = writable_runtime(state, connection_id, write_command).await?;
    capabilities::require(&runtime, CapabilityId::Scripting)?;
    let mut command = redis::cmd("EVAL");
    command
        .arg(CREATE_SCRIPT)
        .arg(1)
        .arg(key)
        .arg(write_command)
        .arg(mode)
        .arg(milliseconds)
        .arg(args);
    let result = runtime.executor.lock().await.query_raw(command).await;
    capabilities::guard(
        state,
        connection_id,
        &runtime,
        CapabilityId::Scripting,
        result,
    )
    .map_err(map_error)?;
    Ok(KeyWriteResult {
        key_type: kind.into(),
    })
}

// 原子取得序列化数据与绝对过期时间。脚本只读，可用于应用的只读源会话。
// MEMORY 预检是保守限制，DUMP 后还需检查真实长度；ACL 禁止 MEMORY 时仍允许复制。
pub(crate) const SNAPSHOT_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then return redis.error_reply('SOURCE_MISSING') end
local size = redis.pcall('MEMORY', 'USAGE', KEYS[1])
if type(size) == 'number' and size > tonumber(ARGV[1]) then return redis.error_reply('KEY_TOO_LARGE') end
local data = redis.call('DUMP', KEYS[1])
if not data then return redis.error_reply('SOURCE_MISSING') end
if #data > tonumber(ARGV[1]) then return redis.error_reply('KEY_TOO_LARGE') end
local ttl = redis.call('PTTL', KEYS[1])
local expiry = 0
if ttl >= 0 then
    local time = redis.call('TIME')
    expiry = time[1] * 1000 + math.floor(time[2] / 1000) + ttl
end
return {redis.call('TYPE', KEYS[1]).ok, data, string.format('%.0f', expiry), ttl}
"#;

// RESTORE 自带“不覆盖”；和到期检查放在同一个单 key 脚本中。
pub(crate) const RESTORE_SCRIPT: &str = r#"
local time = redis.call('TIME')
local now = time[1] * 1000 + math.floor(time[2] / 1000)
local expiry = tonumber(ARGV[3])
if ARGV[2] == 'preserve' then
    expiry = tonumber(ARGV[4])
    if expiry > 0 and expiry <= now then return redis.error_reply('SOURCE_EXPIRED') end
elseif ARGV[2] == 'relative' then
    expiry = now + expiry
elseif ARGV[2] == 'absolute' and expiry <= now then
    return redis.error_reply('INVALID_EXPIRY')
end
return redis.call('RESTORE', KEYS[1], string.format('%.0f', expiry), ARGV[1], 'ABSTTL')
"#;

pub(crate) fn same_server(a: &ConnectionConfig, b: &ConnectionConfig) -> bool {
    a.host == b.host
        && a.port == b.port
        && a.auth == b.auth
        && a.username == b.username
        && a.cluster.unwrap_or(false) == b.cluster.unwrap_or(false)
        && a.ssl_options == b.ssl_options
        && a.ssh_options == b.ssh_options
        && a.sentinel_options == b.sentinel_options
}

/// 同步克隆：跑与任务同一个 `Run::transfer`，但不注册任务、不报进度、不能取消。
///
/// 不暴露成 Tauri 命令——界面一律走 `redis_start_clone_key`，一个没有调用方的同步入口
/// 只会是绕开任务语义（取消、`uncertain` 终态）的后门。它的用户是 e2e：同步拿到结果
/// 才好逐项断言传输路径。
pub async fn clone_key(
    state: &AppState,
    source_connection_id: &str,
    target_connection_id: &str,
    source_key: &str,
    target_key: &str,
    expiry: KeyExpiry,
) -> AppResult<KeyWriteResult> {
    super::clone_task::clone_inline(
        state,
        source_connection_id,
        target_connection_id,
        source_key,
        target_key,
        expiry,
    )
    .await
}
