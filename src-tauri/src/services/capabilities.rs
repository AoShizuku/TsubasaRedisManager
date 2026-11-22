//! 服务端身份与能力探测。见 doc/backend/capabilities.md。
//!
//! 三个维度分开：**身份**（`INFO server`）、**能力**（`COMMAND INFO` + 静态注册表）、
//! **权限**（不预检，执行时的 `NOPERM` 映射成 `AppError::PermissionDenied`）。
//! 这里只管前两个。

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::state::{AppState, RedisSessionRuntime};

/* ── 身份 ─────────────────────────────────────────────── */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerFlavor {
    Redis,
    Valkey,
    Dragonfly,
    Keydb,
    Unknown,
}

impl ServerFlavor {
    fn display(self) -> &'static str {
        match self {
            ServerFlavor::Redis => "Redis",
            ServerFlavor::Valkey => "Valkey",
            ServerFlavor::Dragonfly => "Dragonfly",
            ServerFlavor::Keydb => "KeyDB",
            ServerFlavor::Unknown => "未识别的服务端",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerIdentity {
    pub flavor: ServerFlavor,
    /// 实现自己的版本号。能力判定只看它。
    pub version: Option<String>,
    /// `redis_version` 原值，只用于展示与排障 —— 兼容实现在这里报的是兼容声明，不是能力集。
    pub compat_version: Option<String>,
}

impl ServerIdentity {
    pub fn unknown() -> Self {
        Self {
            flavor: ServerFlavor::Unknown,
            version: None,
            compat_version: None,
        }
    }

    /// 按字段判定实现，不按版本号猜。判据均为实测（`tests/redis_capabilities.rs`）：
    ///
    /// - Valkey 8 起 `redis_version` 固定报 `7.2.4`，真实版本在 `valkey_version`，另有 `server_name:valkey`
    /// - Dragonfly 带 `dragonfly_version:df-v2.0.2`，`redis_version` 是兼容声明
    /// - KeyDB 6.3 的版本就写在 `redis_version`，`INFO server` 里唯一的特有字段是 `features`
    ///   （Redis 与其他实现都没有这一行）。没有这一行的老 KeyDB 会被认成 Redis 6.x ——
    ///   它本来就是 Redis 6 的分支，能力判定的结论一致
    pub fn parse(info: &str) -> Self {
        let fields: HashMap<&str, &str> = info
            .lines()
            .filter_map(|line| line.trim().split_once(':'))
            .map(|(name, value)| (name.trim(), value.trim()))
            .filter(|(_, value)| !value.is_empty())
            .collect();
        let field = |name: &str| fields.get(name).map(|value| value.to_string());
        let compat_version = field("redis_version");

        let (flavor, version) = if fields.get("server_name") == Some(&"valkey")
            || fields.contains_key("valkey_version")
        {
            (ServerFlavor::Valkey, field("valkey_version"))
        } else if let Some(raw) = fields.get("dragonfly_version") {
            let trimmed = raw.trim_start_matches("df-").trim_start_matches('v');
            (ServerFlavor::Dragonfly, Some(trimmed.to_string()))
        } else if fields.contains_key("features") && compat_version.is_some() {
            (ServerFlavor::Keydb, compat_version.clone())
        } else if compat_version.is_some() {
            (ServerFlavor::Redis, compat_version.clone())
        } else {
            return Self::unknown();
        };

        Self {
            flavor,
            version,
            compat_version,
        }
    }

    /// 「Valkey 8.1.1（兼容声明 redis_version 7.2.4）」。界面上一律用它，不要自己拼「Redis x」。
    pub fn label(&self) -> String {
        let mut label = self.flavor.display().to_string();
        if let Some(version) = &self.version {
            label.push(' ');
            label.push_str(version);
        }
        if let Some(compat) = &self.compat_version {
            if self.flavor != ServerFlavor::Redis && self.version.as_ref() != Some(compat) {
                label.push_str(&format!("（兼容声明 redis_version {compat}）"));
            }
        }
        label
    }

    fn version_number(&self) -> Option<(u32, u32)> {
        let mut parts = self.version.as_deref()?.split('.');
        let major = parts.next()?.trim().parse().ok()?;
        let minor = parts
            .next()
            .and_then(|part| part.trim().parse().ok())
            .unwrap_or(0);
        Some((major, minor))
    }
}

/* ── 能力 ─────────────────────────────────────────────── */

/// 注册表的键。TS 侧是 `ipc.ts` 的 `CAPABILITY_IDS`，两边手改。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CapabilityId {
    ExpireConditions,
    SetKeepTtl,
    Scripting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CapabilityState {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub state: CapabilityState,
    /// 按登记项的「未知时」策略折算后的结论。前端只认它，不再自己决定 unknown 算什么。
    pub usable: bool,
    /// 界面原样显示。判断依据只有后端知道，所以文案也由后端生成。
    pub reason: String,
}

pub type Capabilities = BTreeMap<CapabilityId, Capability>;

enum Criterion {
    /// `COMMAND INFO` 里存在即支持。
    Command(&'static str),
    /// 命令存在，且该实现的版本不低于 `since` 里登记的值。没登记的实现得 unknown ——
    /// 没把握就不填，不要硬编一个版本号。
    Argument {
        command: &'static str,
        since: &'static [(ServerFlavor, Since)],
    },
}

#[derive(Clone, Copy)]
enum Since {
    /// 该实现的所有版本都有（例如 Valkey 从 Redis 7.2.4 分叉，7.0 的参数全都在）。
    Always,
    Version(u32, u32),
}

/// 判据只有一条：**不支持时会不会静默做错事**。会，就 `Disable`；只会大声报错，就可以 `Allow`。
#[derive(Clone, Copy, PartialEq, Eq)]
enum WhenUnknown {
    Disable,
    Allow,
}

struct Entry {
    id: CapabilityId,
    /// 出现在 `reason` 里的功能名。
    label: &'static str,
    criterion: Criterion,
    when_unknown: WhenUnknown,
}

/// 每一项随它的第一个用户一起登记；没人用的条目没人验证，迟早写错。
const REGISTRY: &[Entry] = &[
    // 用户：批量 TTL 的条件 flag。低版本收到直接报错，但界面要提前说而不是等报错 ——
    // 未知时禁用：挡住的代价只是少一个选项。
    Entry {
        id: CapabilityId::ExpireConditions,
        label: "条件设置",
        criterion: Criterion::Argument {
            command: "EXPIRE",
            since: &[
                (ServerFlavor::Redis, Since::Version(7, 0)),
                (ServerFlavor::Valkey, Since::Always),
            ],
        },
        when_unknown: WhenUnknown::Disable,
    },
    // 用户：STRING 保存。未知时允许：一律禁用会把认不出的兼容实现上的保存整个挡死，
    // 而服务端真不支持时 SET 会报语法错误，不会静默清掉 TTL。
    Entry {
        id: CapabilityId::SetKeepTtl,
        label: "保存时保留过期时间（KEEPTTL）",
        criterion: Criterion::Argument {
            command: "SET",
            since: &[
                (ServerFlavor::Redis, Since::Version(6, 0)),
                (ServerFlavor::Valkey, Since::Always),
            ],
        },
        when_unknown: WhenUnknown::Allow,
    },
    // 用户：新建 / 克隆 / List 按下标删除，以及 KEEPTTL 不可用时的保存。
    // 未知时允许：这些是基础路径，被拒时 EVAL 会大声报错。
    Entry {
        id: CapabilityId::Scripting,
        label: "Lua 脚本",
        criterion: Criterion::Command("EVAL"),
        when_unknown: WhenUnknown::Allow,
    },
];

fn entry(id: CapabilityId) -> &'static Entry {
    REGISTRY
        .iter()
        .find(|entry| entry.id == id)
        .expect("every CapabilityId is registered")
}

/// 建连时 `COMMAND INFO` 要问的命令：注册表引用到的，去重。静态、有限，回复体积有上界。
pub fn probed_commands() -> Vec<&'static str> {
    let mut names = Vec::new();
    for entry in REGISTRY {
        let name = match entry.criterion {
            Criterion::Command(name) => name,
            Criterion::Argument { command, .. } => command,
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// `COMMAND INFO c1 … cN` 的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandProbe {
    /// 名单里存在的命令（大写）。回复里是 nil 的不在这里：不存在，或已被 `rename-command` 改名 ——
    /// 按原名发不出去，这个结论对功能路径是对的。
    Listed(Vec<&'static str>),
    /// `COMMAND` 被 ACL 拒绝。这不代表服务端不支持什么。
    Denied,
    /// 其他原因拿不到（被改名、兼容实现不支持、回复形状不对）。
    Unavailable,
}

impl CommandProbe {
    pub fn parse(names: &[&'static str], reply: Option<&redis::Value>) -> Self {
        match reply {
            Some(redis::Value::Array(items)) if items.len() == names.len() => CommandProbe::Listed(
                names
                    .iter()
                    .zip(items)
                    .filter(|(_, item)| !matches!(item, redis::Value::Nil))
                    .map(|(name, _)| *name)
                    .collect(),
            ),
            Some(redis::Value::ServerError(error))
                if error.kind() == Some(redis::ServerErrorKind::NoPerm) =>
            {
                CommandProbe::Denied
            }
            _ => CommandProbe::Unavailable,
        }
    }

    fn has(&self, command: &str) -> Option<bool> {
        match self {
            CommandProbe::Listed(present) => Some(present.contains(&command)),
            CommandProbe::Denied | CommandProbe::Unavailable => None,
        }
    }
}

pub fn evaluate(identity: &ServerIdentity, probe: &CommandProbe) -> Capabilities {
    REGISTRY
        .iter()
        .map(|entry| (entry.id, evaluate_entry(entry, identity, probe)))
        .collect()
}

fn evaluate_entry(entry: &Entry, identity: &ServerIdentity, probe: &CommandProbe) -> Capability {
    let label = entry.label;
    match entry.criterion {
        Criterion::Command(command) => match probe.has(command) {
            Some(true) => supported(format!("服务端提供 `{command}`，{label}可用")),
            Some(false) => missing(command, label),
            None => unknown(entry, probe_unavailable(probe, label)),
        },
        Criterion::Argument { command, since } => {
            // COMMAND INFO 只能把结论往「不支持」推：命令被改名了，参数再新也发不出去。
            // 它拿不到时不改变结论 —— 参数级能力本质上是「某版本起」，判据是版本号。
            if probe.has(command) == Some(false) {
                return missing(command, label);
            }
            let Some(version) = identity.version_number() else {
                return unknown(
                    entry,
                    format!(
                        "读不到服务端版本（INFO 可能被 ACL 或 rename-command 拒绝），{label}是否可用无法确认"
                    ),
                );
            };
            let server = identity.label();
            match since
                .iter()
                .find(|(flavor, _)| *flavor == identity.flavor)
                .map(|(_, since)| *since)
            {
                None => unknown(entry, format!("{server}，{label}是否可用无法确认")),
                Some(Since::Always) => supported(format!("{server}，{label}可用")),
                Some(Since::Version(major, minor)) if version >= (major, minor) => {
                    supported(format!("{server}，{label}可用"))
                }
                Some(Since::Version(major, minor)) => Capability {
                    state: CapabilityState::Unsupported,
                    usable: false,
                    reason: format!(
                        "{server}，{label}需要 {} {major}.{minor} 及以上",
                        identity.flavor.display()
                    ),
                },
            }
        }
    }
}

fn supported(reason: String) -> Capability {
    Capability {
        state: CapabilityState::Supported,
        usable: true,
        reason,
    }
}

fn missing(command: &str, label: &str) -> Capability {
    Capability {
        state: CapabilityState::Unsupported,
        usable: false,
        reason: format!("`{command}` 不存在或已被改名，{label}不可用"),
    }
}

fn unknown(entry: &Entry, why: String) -> Capability {
    let usable = entry.when_unknown == WhenUnknown::Allow;
    let policy = if usable {
        "，照常尝试，服务端不支持时会报错"
    } else {
        "，按不支持处理"
    };
    Capability {
        state: CapabilityState::Unknown,
        usable,
        reason: format!("{why}{policy}"),
    }
}

fn probe_unavailable(probe: &CommandProbe, label: &str) -> String {
    match probe {
        CommandProbe::Denied => format!("`COMMAND` 被 ACL 拒绝，{label}是否可用无法确认"),
        _ => format!("`COMMAND INFO` 不可用，{label}是否可用无法确认"),
    }
}

/* ── 运行时降级 ───────────────────────────────────────── */

/// 会话能力变化的推送，经 `redis://capability-change` 到前端。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityChangeEvent {
    pub connection_id: String,
    pub id: CapabilityId,
    pub capability: Capability,
}

/// 受保护的命令回了这些错误，说明建连时的判断错了（判据写错、集群节点不一致、兼容实现谎报）。
///
/// 只认「命令或参数不被认识」这一类。脚本里的业务错误、`NOPERM`、`WRONGTYPE` 都不算 ——
/// 它们与能力无关，降级会把一个可用的功能在整个会话里置灰。
pub fn looks_unsupported(id: CapabilityId, detail: &str) -> bool {
    let detail = detail.to_ascii_lowercase();
    if detail.contains("unknown command") {
        return true;
    }
    match entry(id).criterion {
        Criterion::Command(_) => false,
        Criterion::Argument { .. } => {
            detail.contains("syntax error") || detail.contains("wrong number of arguments")
        }
    }
}

/// 会话内把这一项降为 unsupported，推给前端，并给出本次操作的错误。
///
/// **不自动重试、不换路径重做**：用户点的是保存，换一种方式悄悄执行不是他确认过的东西。
fn degrade(
    state: &AppState,
    connection_id: &str,
    runtime: &RedisSessionRuntime,
    id: CapabilityId,
    detail: String,
) -> AppError {
    let label = entry(id).label;
    let capability = Capability {
        state: CapabilityState::Unsupported,
        usable: false,
        reason: format!("{label}被服务端拒绝（命令或参数不被识别），本会话内按不支持处理"),
    };
    runtime.set_capability(id, capability.clone());
    if let Some(sink) = state.capability_sink.get() {
        sink(CapabilityChangeEvent {
            connection_id: connection_id.to_string(),
            id,
            capability,
        });
    }
    AppError::CapabilityUnsupported {
        message: format!("{label}不被服务端支持，本次操作没有执行"),
        detail: Some(detail),
    }
}

/// 功能入口的守卫：前端本应已经置灰，这是后端一侧的同一条规则。
pub fn require(runtime: &RedisSessionRuntime, id: CapabilityId) -> AppResult<()> {
    let capability = runtime.capability(id);
    if capability.usable {
        return Ok(());
    }
    Err(AppError::CapabilityUnsupported {
        message: capability.reason,
        detail: None,
    })
}

/// 受能力保护的命令的结果：服务端回了「不认识」类错误就降级该项（§ 运行时降级），
/// 其余原样返回。
pub fn guard<T>(
    state: &AppState,
    connection_id: &str,
    runtime: &RedisSessionRuntime,
    id: CapabilityId,
    result: AppResult<T>,
) -> AppResult<T> {
    match result {
        Err(AppError::RedisCommandFailed(detail)) if looks_unsupported(id, &detail) => {
            Err(degrade(state, connection_id, runtime, id, detail))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REDIS_6: &str =
        "# Server\r\nredis_version:6.2.14\r\nredis_git_sha1:00000000\r\nredis_mode:standalone\r\n";
    const REDIS_7: &str = "# Server\r\nredis_version:7.2.4\r\nredis_mode:standalone\r\n";
    // 以下样本取自真实服务端（valkey/valkey:8-alpine、dragonflydb、eqalpha/keydb）
    const VALKEY_8: &str = "# Server\r\nredis_version:7.2.4\r\nserver_name:valkey\r\nvalkey_version:8.1.10\r\nvalkey_release_stage:ga\r\n";
    const DRAGONFLY: &str =
        "# Server\r\nredis_version:7.4.0\r\ndragonfly_version:df-v2.0.2\r\nredis_mode:standalone\r\n";
    const KEYDB: &str = "# Server\r\nredis_version:6.3.4\r\nredis_mode:standalone\r\nexecutable:/data/keydb-server\r\nfeatures:cluster_mget\r\n";

    fn all_listed() -> CommandProbe {
        CommandProbe::Listed(probed_commands())
    }

    fn capability(identity: &ServerIdentity, probe: &CommandProbe, id: CapabilityId) -> Capability {
        evaluate(identity, probe).remove(&id).unwrap()
    }

    #[test]
    fn identifies_each_implementation_by_its_own_fields() {
        let redis = ServerIdentity::parse(REDIS_7);
        assert_eq!(redis.flavor, ServerFlavor::Redis);
        assert_eq!(redis.version.as_deref(), Some("7.2.4"));
        assert_eq!(redis.label(), "Redis 7.2.4");

        let valkey = ServerIdentity::parse(VALKEY_8);
        assert_eq!(valkey.flavor, ServerFlavor::Valkey);
        assert_eq!(valkey.version.as_deref(), Some("8.1.10"));
        assert_eq!(valkey.compat_version.as_deref(), Some("7.2.4"));
        assert_eq!(
            valkey.label(),
            "Valkey 8.1.10（兼容声明 redis_version 7.2.4）"
        );

        let dragonfly = ServerIdentity::parse(DRAGONFLY);
        assert_eq!(dragonfly.flavor, ServerFlavor::Dragonfly);
        assert_eq!(dragonfly.version.as_deref(), Some("2.0.2"));

        let keydb = ServerIdentity::parse(KEYDB);
        assert_eq!(keydb.flavor, ServerFlavor::Keydb);
        assert_eq!(keydb.version.as_deref(), Some("6.3.4"));
        assert_eq!(keydb.label(), "KeyDB 6.3.4");
    }

    #[test]
    fn missing_blank_or_empty_info_reads_as_unknown_identity() {
        assert_eq!(ServerIdentity::parse(""), ServerIdentity::unknown());
        assert_eq!(
            ServerIdentity::parse("# Server\r\nos:Linux\r\n"),
            ServerIdentity::unknown()
        );
        assert_eq!(
            ServerIdentity::parse("redis_version:\r\n"),
            ServerIdentity::unknown()
        );
        // 行首缩进与 \r 不能留在值里，否则会污染版本号比较
        let padded = ServerIdentity::parse("  redis_version: 6.2.14 \r\n");
        assert_eq!(padded.version.as_deref(), Some("6.2.14"));
        // 别被前缀相同的字段骗了
        assert_eq!(
            ServerIdentity::parse("redis_version_extra:1.2.3\r\n"),
            ServerIdentity::unknown()
        );
    }

    #[test]
    fn valkey_is_judged_by_its_own_version_not_the_compat_declaration() {
        // Valkey 9 有 Redis 7.4 才有的东西；按 redis_version 7.2.4 判会判反。
        // 这里换个方向验证：Valkey 的 since 是 Always，与 compat 版本无关。
        let valkey = ServerIdentity::parse(VALKEY_8);
        let cap = capability(&valkey, &all_listed(), CapabilityId::ExpireConditions);
        assert_eq!(cap.state, CapabilityState::Supported);
        assert_eq!(
            cap.reason,
            "Valkey 8.1.10（兼容声明 redis_version 7.2.4），条件设置可用"
        );
    }

    #[test]
    fn argument_capabilities_follow_the_since_table() {
        let redis6 = ServerIdentity::parse(REDIS_6);
        let expire = capability(&redis6, &all_listed(), CapabilityId::ExpireConditions);
        assert_eq!(expire.state, CapabilityState::Unsupported);
        assert!(!expire.usable);
        assert_eq!(expire.reason, "Redis 6.2.14，条件设置需要 Redis 7.0 及以上");
        let keep = capability(&redis6, &all_listed(), CapabilityId::SetKeepTtl);
        assert_eq!(keep.state, CapabilityState::Supported);

        let redis5 = ServerIdentity::parse("redis_version:5.0.14\r\n");
        let keep = capability(&redis5, &all_listed(), CapabilityId::SetKeepTtl);
        assert_eq!(keep.state, CapabilityState::Unsupported);
    }

    #[test]
    fn a_flavor_missing_from_since_is_unknown_never_supported() {
        // KeyDB 6.3 实测有 KEEPTTL、没有 EXPIRE NX，但它不在 since 表里 —— 没把握就是 unknown
        let keydb = ServerIdentity::parse(KEYDB);
        let expire = capability(&keydb, &all_listed(), CapabilityId::ExpireConditions);
        assert_eq!(expire.state, CapabilityState::Unknown);
        assert!(!expire.usable, "expireConditions 未知时禁用");
        assert_eq!(
            expire.reason,
            "KeyDB 6.3.4，条件设置是否可用无法确认，按不支持处理"
        );

        let keep = capability(&keydb, &all_listed(), CapabilityId::SetKeepTtl);
        assert_eq!(keep.state, CapabilityState::Unknown);
        assert!(keep.usable, "setKeepTtl 未知时允许");
    }

    #[test]
    fn unknown_identity_leaves_argument_capabilities_unknown() {
        let cap = capability(
            &ServerIdentity::unknown(),
            &all_listed(),
            CapabilityId::ExpireConditions,
        );
        assert_eq!(cap.state, CapabilityState::Unknown);
        assert!(cap.reason.starts_with("读不到服务端版本"));
    }

    #[test]
    fn command_info_reply_maps_to_three_states() {
        let names = probed_commands();
        assert_eq!(names, vec!["EXPIRE", "SET", "EVAL"]);

        let info = redis::Value::Array(vec![redis::Value::Int(1)]);
        let reply = redis::Value::Array(vec![info.clone(), info, redis::Value::Nil]);
        let probe = CommandProbe::parse(&names, Some(&reply));
        assert_eq!(probe, CommandProbe::Listed(vec!["EXPIRE", "SET"]));

        let redis7 = ServerIdentity::parse(REDIS_7);
        let scripting = capability(&redis7, &probe, CapabilityId::Scripting);
        assert_eq!(scripting.state, CapabilityState::Unsupported);
        assert_eq!(scripting.reason, "`EVAL` 不存在或已被改名，Lua 脚本不可用");
    }

    #[test]
    fn a_renamed_command_makes_its_argument_capability_unsupported() {
        let names = probed_commands();
        let info = redis::Value::Array(vec![redis::Value::Int(1)]);
        let reply = redis::Value::Array(vec![redis::Value::Nil, info.clone(), info]);
        let probe = CommandProbe::parse(&names, Some(&reply));
        let cap = capability(
            &ServerIdentity::parse(REDIS_7),
            &probe,
            CapabilityId::ExpireConditions,
        );
        assert_eq!(cap.state, CapabilityState::Unsupported);
    }

    /// `ServerError` 的内部表示是 crate 私有的，借解析器从 RESP 字节造一格。
    fn server_error(line: &str) -> redis::Value {
        redis::parse_redis_value(format!("-{line}\r\n").as_bytes()).unwrap()
    }

    #[test]
    fn a_rejected_command_probe_is_unknown_not_unsupported() {
        let names = probed_commands();
        let redis7 = ServerIdentity::parse(REDIS_7);

        let denied =
            server_error("NOPERM User viewer has no permissions to run the 'command|info' command");
        let probe = CommandProbe::parse(&names, Some(&denied));
        assert_eq!(probe, CommandProbe::Denied);
        let scripting = capability(&redis7, &probe, CapabilityId::Scripting);
        assert_eq!(scripting.state, CapabilityState::Unknown);
        assert!(scripting.usable, "scripting 未知时允许");
        assert_eq!(
            scripting.reason,
            "`COMMAND` 被 ACL 拒绝，Lua 脚本是否可用无法确认，照常尝试，服务端不支持时会报错"
        );
        // 参数级能力的判据是版本号，COMMAND 被拒不改变结论
        let expire = capability(&redis7, &probe, CapabilityId::ExpireConditions);
        assert_eq!(expire.state, CapabilityState::Supported);

        let renamed = server_error("ERR unknown command 'COMMAND'");
        assert_eq!(
            CommandProbe::parse(&names, Some(&renamed)),
            CommandProbe::Unavailable
        );
        assert_eq!(CommandProbe::parse(&names, None), CommandProbe::Unavailable);
        // 条数对不上就不信它，宁可 unknown
        let short = redis::Value::Array(vec![redis::Value::Nil]);
        assert_eq!(
            CommandProbe::parse(&names, Some(&short)),
            CommandProbe::Unavailable
        );
    }

    #[test]
    fn only_unrecognised_command_or_argument_replies_trigger_degradation() {
        let keep = CapabilityId::SetKeepTtl;
        assert!(looks_unsupported(keep, "ERR syntax error"));
        assert!(looks_unsupported(
            CapabilityId::ExpireConditions,
            "ERR wrong number of arguments for 'expire' command"
        ));
        assert!(looks_unsupported(
            CapabilityId::Scripting,
            "ERR unknown command 'EVAL', with args beginning with: "
        ));
        // 命令级能力只认「命令不认识」：脚本里的语法错误是脚本的事，不是服务端缺能力
        assert!(!looks_unsupported(
            CapabilityId::Scripting,
            "ERR Error compiling script: syntax error"
        ));
        assert!(!looks_unsupported(keep, "NOPERM User u has no permissions"));
        assert!(!looks_unsupported(
            keep,
            "WRONGTYPE Operation against a key"
        ));
    }
}
