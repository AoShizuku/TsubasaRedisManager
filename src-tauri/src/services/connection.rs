use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use crate::error::{AppError, AppResult};
use crate::models::{
    CommandResult, ConnectionConfig, ConnectionOpened, KeyEntry, KeyListResult, KeyMeta, SshOptions,
};
use crate::services::capabilities::{self, Capabilities, CommandProbe, ServerIdentity};
use crate::services::config::{build_connection_plan, ConnectionMode};
use crate::services::executor::{PipelineTarget, RedisExecutor};
use crate::services::{cli, cluster, hll, readonly, redis_values, sentinel, ssh, tls};
use crate::state::{
    AppState, RedisSession, RedisSessionRuntime, RedisSessionRuntimeHandle, SessionResource,
};
use redis::{FromRedisValue, IntoConnectionInfo, RedisConnectionInfo};
use tokio::sync::Mutex;

pub async fn open_connection(
    state: &AppState,
    config: ConnectionConfig,
) -> AppResult<ConnectionOpened> {
    open_connection_with_timeout(state, config, REDIS_RESPONSE_TIMEOUT).await
}

/// 克隆独占自己的连接、SSH 隧道与响应预算，不占住浏览会话的 executor。
pub(crate) async fn open_transfer_runtime(
    config: &ConnectionConfig,
) -> AppResult<RedisSessionRuntimeHandle> {
    let isolated = AppState::default();
    let opened = open_connection_with_timeout(
        &isolated,
        config.clone(),
        std::time::Duration::from_secs(300),
    )
    .await?;
    session_runtime(&isolated, &opened.connection_id).await
}

async fn open_connection_with_timeout(
    state: &AppState,
    config: ConnectionConfig,
    response_timeout: std::time::Duration,
) -> AppResult<ConnectionOpened> {
    let plan = build_connection_plan(&config)?;
    let connection_name = plan.connection_name.clone();
    let readonly = plan.readonly;
    if !is_supported_connection_mode(&plan.mode) {
        return Err(AppError::ConnectionConfigInvalid(format!(
            "Connection mode {:?} is not implemented yet",
            plan.mode
        )));
    }
    let mut resources = Vec::new();
    let mut effective_config = config.clone();
    if uses_seed_ssh_tunnel(&plan.mode) {
        let ssh_options = require_ssh_options(&config)?;
        let tunnel = ssh::open_tunnel(
            ssh_options,
            ssh::SshTarget {
                host: config.host.clone(),
                port: config.port,
            },
        )
        .await?;
        effective_config.host = tunnel.local_host.clone();
        effective_config.port = tunnel.local_port;
        resources.push(SessionResource::SshTunnel(tunnel));
    }

    if is_sentinel_mode(&plan.mode) {
        let sentinel_options = effective_config.sentinel_options.clone().ok_or_else(|| {
            AppError::ConnectionConfigInvalid("Sentinel options are required".into())
        })?;
        let mut sentinel_connection = open_sentinel_connection(&effective_config).await?;
        let master = match &mut sentinel_connection {
            SentinelConnection::Manager(manager) => {
                sentinel::resolve_master(manager, &sentinel_options.master_name).await?
            }
            SentinelConnection::Multiplexed(connection) => {
                sentinel::resolve_master(connection, &sentinel_options.master_name).await?
            }
        };
        if is_sentinel_ssh_mode(&plan.mode) {
            let ssh_options = require_ssh_options(&config)?;
            let tunnel = ssh::open_tunnel(
                ssh_options,
                ssh::SshTarget {
                    host: master.host,
                    port: master.port,
                },
            )
            .await?;
            effective_config.host = tunnel.local_host.clone();
            effective_config.port = tunnel.local_port;
            resources.push(SessionResource::SshTunnel(tunnel));
        } else {
            effective_config.host = master.host;
            effective_config.port = master.port;
        }
        if let Some(node_password) = non_empty_after_trim(sentinel_options.node_password.as_deref())
        {
            effective_config.auth = Some(node_password.to_string());
        }
    }

    let mut executor = match &plan.mode {
        ConnectionMode::Cluster | ConnectionMode::ClusterTls => {
            let nodes = vec![build_cluster_seed_url(&effective_config)];
            RedisExecutor::Cluster(
                cluster::open_cluster_connection_with_timeout(
                    nodes,
                    &effective_config,
                    None,
                    response_timeout,
                )
                .await?,
            )
        }
        ConnectionMode::ClusterSsh | ConnectionMode::ClusterTlsOverSsh => {
            build_cluster_ssh_executor(&config, &effective_config, &mut resources, response_timeout)
                .await?
        }
        _ => build_standalone_executor_with_timeout(&effective_config, response_timeout).await?,
    };

    ping_executor(&mut executor).await?;
    let (server, capabilities) = probe_server(&mut executor).await;

    let connection_id = uuid::Uuid::new_v4().to_string();
    state.sessions.lock().await.insert(
        connection_id.clone(),
        RedisSession {
            connection_name: connection_name.clone(),
            readonly,
            mode: plan.mode,
            config: config.clone(),
            runtime: Arc::new(RedisSessionRuntime {
                executor: Arc::new(Mutex::new(executor)),
                resources,
                capabilities: std::sync::Mutex::new(capabilities.clone()),
            }),
        },
    );

    Ok(ConnectionOpened {
        connection_id,
        connection_name,
        readonly,
        server,
        capabilities,
    })
}

/// 身份与能力，建连时一个 pipeline：`INFO server` + `COMMAND INFO c1 … cN`。
/// 见 doc/backend/capabilities.md「探测」。
///
/// **探测失败绝不让连接失败** —— `INFO` / `COMMAND` 会被 ACL 或 `rename-command` 拒掉，
/// 那不是连不上的理由；拿不到的部分落成 unknown，由各登记项的「未知时」策略接手。
async fn probe_server(executor: &mut RedisExecutor) -> (ServerIdentity, Capabilities) {
    let names = capabilities::probed_commands();
    let mut pipeline = redis::pipe();
    pipeline.cmd("INFO").arg("server");
    let command_info = pipeline.cmd("COMMAND").arg("INFO");
    for name in &names {
        command_info.arg(*name);
    }
    let replies = executor
        .query_pipeline_raw(pipeline, PipelineTarget::AnyNode)
        .await
        .unwrap_or_default();

    let server = redis_values::optional_bytes(replies.first())
        .map(|bytes| ServerIdentity::parse(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_else(ServerIdentity::unknown);
    let probe = CommandProbe::parse(&names, replies.get(1));
    let capabilities = capabilities::evaluate(&server, &probe);
    (server, capabilities)
}

fn is_supported_connection_mode(mode: &ConnectionMode) -> bool {
    matches!(
        mode,
        ConnectionMode::Standalone
            | ConnectionMode::StandaloneTls
            | ConnectionMode::StandaloneSsh
            | ConnectionMode::StandaloneTlsOverSsh
            | ConnectionMode::Cluster
            | ConnectionMode::ClusterTls
            | ConnectionMode::ClusterSsh
            | ConnectionMode::ClusterTlsOverSsh
            | ConnectionMode::Sentinel
            | ConnectionMode::SentinelTls
            | ConnectionMode::SentinelSsh
            | ConnectionMode::SentinelTlsOverSsh
    )
}

fn uses_seed_ssh_tunnel(mode: &ConnectionMode) -> bool {
    matches!(
        mode,
        ConnectionMode::StandaloneSsh
            | ConnectionMode::StandaloneTlsOverSsh
            | ConnectionMode::ClusterSsh
            | ConnectionMode::ClusterTlsOverSsh
            | ConnectionMode::SentinelSsh
            | ConnectionMode::SentinelTlsOverSsh
    )
}

fn is_sentinel_mode(mode: &ConnectionMode) -> bool {
    matches!(
        mode,
        ConnectionMode::Sentinel
            | ConnectionMode::SentinelTls
            | ConnectionMode::SentinelSsh
            | ConnectionMode::SentinelTlsOverSsh
    )
}

fn is_sentinel_ssh_mode(mode: &ConnectionMode) -> bool {
    matches!(
        mode,
        ConnectionMode::SentinelSsh | ConnectionMode::SentinelTlsOverSsh
    )
}

fn uses_resolved_sentinel_master_ssh_tunnel(mode: &ConnectionMode) -> bool {
    is_sentinel_ssh_mode(mode)
}

fn require_ssh_options(config: &ConnectionConfig) -> AppResult<&SshOptions> {
    config
        .ssh_options
        .as_ref()
        .ok_or_else(|| AppError::ConnectionConfigInvalid("SSH options are required".into()))
}

async fn build_cluster_ssh_executor(
    config: &ConnectionConfig,
    effective_config: &ConnectionConfig,
    resources: &mut Vec<SessionResource>,
    response_timeout: std::time::Duration,
) -> AppResult<RedisExecutor> {
    let mut seed_executor = build_standalone_executor(effective_config).await?;
    let cluster_nodes = query_cluster_nodes(&mut seed_executor).await?;
    let masters = cluster::tunnel_targets_for_masters(&cluster_nodes)?;
    if masters.is_empty() {
        return Err(AppError::RedisClusterFailed(
            "No healthy Redis Cluster masters found for SSH fan-out".into(),
        ));
    }

    let ssh_options = require_ssh_options(config)?;
    let mut tunnel_nodes = Vec::with_capacity(masters.len());
    for master in &masters {
        let tunnel = ssh::open_tunnel(
            ssh_options,
            ssh::SshTarget {
                host: master.host.clone(),
                port: master.port,
            },
        )
        .await?;
        tunnel_nodes.push(cluster::ClusterNode {
            host: tunnel.local_host.clone(),
            port: tunnel.local_port,
        });
        resources.push(SessionResource::SshTunnel(tunnel));
    }

    let resolver = cluster::build_tunnel_resolver(&masters, &tunnel_nodes)?;
    let nodes = build_cluster_ssh_seed_urls(&masters, effective_config);

    Ok(RedisExecutor::Cluster(
        cluster::open_cluster_connection_with_timeout(
            nodes,
            effective_config,
            Some(resolver),
            response_timeout,
        )
        .await?,
    ))
}

pub(crate) async fn query_cluster_nodes(executor: &mut RedisExecutor) -> AppResult<String> {
    let value = executor
        .query_raw({
            let mut command = redis::cmd("CLUSTER");
            command.arg("NODES");
            command
        })
        .await
        .map_err(map_cluster_command_error)?;

    String::from_redis_value(value).map_err(|e| AppError::RedisClusterFailed(e.to_string()))
}

fn map_cluster_command_error(error: AppError) -> AppError {
    match error {
        AppError::RedisCommandFailed(detail) => AppError::RedisClusterFailed(detail),
        other => other,
    }
}

enum SentinelConnection {
    Manager(redis::aio::ConnectionManager),
    Multiplexed(redis::aio::MultiplexedConnection),
}

async fn open_sentinel_connection(config: &ConnectionConfig) -> AppResult<SentinelConnection> {
    let connection_info = build_connection_info(config).map_err(map_sentinel_connection_error)?;
    let client = redis::Client::open(connection_info)
        .map_err(|e| AppError::RedisSentinelFailed(e.to_string()))?;

    match sentinel_connection_strategy(config) {
        SentinelConnectionStrategy::Manager => redis::aio::ConnectionManager::new(client)
            .await
            .map(SentinelConnection::Manager)
            .map_err(|e| AppError::RedisSentinelFailed(e.to_string())),
        SentinelConnectionStrategy::MultiplexedWithDnsResolver => {
            let resolver = tls_servername_resolver(config).ok_or_else(|| {
                AppError::RedisSentinelFailed("Sentinel TLS servername resolver is required".into())
            })?;
            let async_config = redis::AsyncConnectionConfig::new().set_dns_resolver(resolver);
            client
                .get_multiplexed_async_connection_with_config(&async_config)
                .await
                .map(SentinelConnection::Multiplexed)
                .map_err(|e| AppError::RedisSentinelFailed(e.to_string()))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SentinelConnectionStrategy {
    Manager,
    MultiplexedWithDnsResolver,
}

fn sentinel_connection_strategy(config: &ConnectionConfig) -> SentinelConnectionStrategy {
    if tls_servername_resolver(config).is_some() {
        SentinelConnectionStrategy::MultiplexedWithDnsResolver
    } else {
        SentinelConnectionStrategy::Manager
    }
}

fn map_sentinel_connection_error(error: AppError) -> AppError {
    match error {
        AppError::RedisConnectionFailed(detail) | AppError::RedisTlsFailed(detail) => {
            AppError::RedisSentinelFailed(detail)
        }
        other => other,
    }
}

pub async fn close_connection(state: &AppState, connection_id: &str) -> AppResult<()> {
    state.sessions.lock().await.remove(connection_id);
    Ok(())
}

struct CommandSession {
    readonly: bool,
    runtime: RedisSessionRuntimeHandle,
}

pub async fn session_config(state: &AppState, connection_id: &str) -> AppResult<ConnectionConfig> {
    let sessions = state.sessions.lock().await;
    sessions
        .get(connection_id)
        .map(|session| session.config.clone())
        .ok_or_else(|| AppError::ConnectionNotFound(connection_id.into()))
}

async fn command_session(state: &AppState, connection_id: &str) -> AppResult<CommandSession> {
    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(connection_id)
        .ok_or_else(|| AppError::ConnectionNotFound(connection_id.into()))?;
    Ok(CommandSession {
        readonly: session.readonly,
        runtime: session.runtime.clone(),
    })
}

pub(crate) async fn session_runtime(
    state: &AppState,
    connection_id: &str,
) -> AppResult<RedisSessionRuntimeHandle> {
    let sessions = state.sessions.lock().await;
    sessions
        .get(connection_id)
        .map(|session| session.runtime.clone())
        .ok_or_else(|| AppError::ConnectionNotFound(connection_id.into()))
}

/// 写类语义化命令的入口守卫。
///
/// 这是 `readonly.rs` 那套命令名匹配的**替代**而不是补充：功能路径的读写性质
/// 在编译期就已经确定，不需要运行时去猜一个字符串是不是写命令。
/// `readonly.rs` 只留给 CLI —— 那里输入是自由文本，只能靠猜。
/// 见 doc/backend/sessions.md「只读属于会话」。
pub(crate) async fn writable_runtime(
    state: &AppState,
    connection_id: &str,
    intent: &str,
) -> AppResult<RedisSessionRuntimeHandle> {
    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(connection_id)
        .ok_or_else(|| AppError::ConnectionNotFound(connection_id.into()))?;
    if session.readonly {
        return Err(AppError::ReadonlyWriteRejected(intent.into()));
    }
    Ok(session.runtime.clone())
}

pub async fn execute_cli(
    state: &AppState,
    connection_id: &str,
    command_line: &str,
) -> AppResult<CommandResult> {
    let args = cli::parse_cli(command_line)?;
    let command = args
        .first()
        .expect("parse_cli returns at least one argument")
        .to_string();
    let session = command_session(state, connection_id).await?;

    if session.readonly && readonly::is_write_command(&command) {
        return Err(AppError::ReadonlyWriteRejected(command));
    }

    let started = Instant::now();
    let mut redis_command = redis::cmd(&command);
    for arg in args.iter().skip(1) {
        redis_command.arg(arg);
    }
    let value = session
        .runtime
        .executor
        .lock()
        .await
        .query(redis_command)
        .await?;

    Ok(CommandResult {
        command,
        value,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// SCAN 本身不返回类型，所以在同一次交互里补一批 `TYPE`（要 TTL 时再补一批 `TTL`）。
///
/// v0.1 是前端补的：`sessions.ts` 的 `ensureTypes` 只给可见行发 `TYPE`，
/// 外加一个 8 路并发队列 —— 那本质是在渲染进程里手搓连接池调度。
/// 一次 pipeline 就够，且滚动时不再有"色点慢慢浮现"的过程。
///
/// `node` 是集群下这批 key 所属的主节点；单机传 `None`。分节点传是必须的：
/// 一批跨槽位的 pipeline 会被整个打到错误的节点上。
///
/// `pub(crate)` 是给扫描任务用的（`task::scan_and_annotate_one_page`）——
/// 任务路径与取值路径必须给出同一形状的注解，注解只有一处实现。
pub(crate) async fn annotate_keys(
    executor: &mut RedisExecutor,
    keys: Vec<String>,
    with_ttl: bool,
    node: Option<(String, u16)>,
) -> AppResult<Vec<KeyEntry>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }

    let mut pipeline = redis::pipe();
    for key in &keys {
        pipeline.cmd("TYPE").arg(key);
    }
    if with_ttl {
        for key in &keys {
            pipeline.cmd("TTL").arg(key);
        }
    }

    let target = match &node {
        Some((host, port)) => PipelineTarget::Node {
            host: host.clone(),
            port: *port,
        },
        // 单机下 target 被忽略，借第一个 key 只是为了满足类型
        None => PipelineTarget::Key(keys[0].as_str()),
    };
    let replies = executor.query_pipeline_raw(pipeline, target).await?;

    let total = keys.len();
    Ok(keys
        .into_iter()
        .enumerate()
        .map(|(index, key)| KeyEntry {
            // SCAN 与 TYPE 之间 key 可能已经被删掉，那时 TYPE 返回 none
            key_type: redis_values::optional_value::<String>(replies.get(index))
                .filter(|key_type| key_type != "none"),
            ttl: if with_ttl {
                redis_values::optional_value::<i64>(replies.get(total + index))
            } else {
                None
            },
            key,
        })
        .collect())
}

pub async fn scan_keys(
    state: &AppState,
    connection_id: &str,
    cursor: &str,
    pattern: Option<String>,
    count: Option<usize>,
    with_ttl: bool,
) -> AppResult<KeyListResult> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;
    if executor.is_cluster() {
        return scan_cluster_keys(&mut executor, cursor, pattern, count, with_ttl).await;
    }

    let mut command = redis::cmd("SCAN");
    command.arg(cursor);
    if let Some(pattern) = pattern.filter(|value| !value.is_empty()) {
        command.arg("MATCH").arg(pattern);
    }
    command.arg("COUNT").arg(count.unwrap_or(200));

    let value = executor.query_raw(command).await?;
    let (next_cursor, keys) = parse_scan_response(value)?;
    let keys = annotate_keys(&mut executor, keys, with_ttl, None).await?;

    Ok(KeyListResult {
        keys,
        completed: next_cursor == "0",
        cursor: next_cursor,
    })
}

async fn scan_cluster_keys(
    executor: &mut RedisExecutor,
    cursor: &str,
    pattern: Option<String>,
    count: Option<usize>,
    with_ttl: bool,
) -> AppResult<KeyListResult> {
    let nodes = query_cluster_nodes(executor).await?;
    let masters = cluster::parse_master_nodes(&nodes)?;
    let cursors = decode_cluster_scan_cursor(cursor, &masters)?;
    let is_initial_scan = cursor == "0";
    let mut next_cursors = HashMap::new();
    let mut keys = Vec::new();

    for master in masters {
        let endpoint = master.endpoint();
        let node_cursor = cursors.get(&endpoint).map(String::as_str).unwrap_or("0");
        if !is_initial_scan && node_cursor == "0" {
            continue;
        }

        let mut command = redis::cmd("SCAN");
        command.arg(node_cursor);
        if let Some(pattern) = pattern.as_deref().filter(|value| !value.is_empty()) {
            command.arg("MATCH").arg(pattern);
        }
        command.arg("COUNT").arg(count.unwrap_or(200));

        let value = executor
            .query_cluster_node_raw(command, master.host.clone(), master.port)
            .await?;
        let (next_cursor, node_keys) = parse_scan_response(value)?;

        // 这一页的 key 全部落在该节点持有的槽位上，所以整批打给它是安全的
        let mut node_entries = annotate_keys(
            executor,
            node_keys,
            with_ttl,
            Some((master.host.clone(), master.port)),
        )
        .await?;
        keys.append(&mut node_entries);

        if next_cursor != "0" {
            next_cursors.insert(endpoint, next_cursor);
        }
    }

    Ok(KeyListResult {
        keys,
        completed: next_cursors.is_empty(),
        cursor: encode_cluster_scan_cursor(&next_cursors),
    })
}

const CLUSTER_SCAN_CURSOR_PREFIX: &str = "cluster:";

pub(crate) fn decode_cluster_scan_cursor(
    cursor: &str,
    masters: &[cluster::ClusterNode],
) -> AppResult<HashMap<String, String>> {
    if cursor == "0" {
        return Ok(masters
            .iter()
            .map(|master| (master.endpoint(), "0".to_string()))
            .collect());
    }

    let Some(raw) = cursor.strip_prefix(CLUSTER_SCAN_CURSOR_PREFIX) else {
        return Err(AppError::RedisCommandFailed(
            "Invalid Redis Cluster scan cursor".into(),
        ));
    };
    serde_json::from_str(raw).map_err(|err| {
        AppError::RedisCommandFailed(format!("Invalid Redis Cluster scan cursor: {err}"))
    })
}

pub(crate) fn encode_cluster_scan_cursor(cursors: &HashMap<String, String>) -> String {
    if cursors.is_empty() {
        return "0".into();
    }

    format!(
        "{CLUSTER_SCAN_CURSOR_PREFIX}{}",
        serde_json::to_string(cursors).expect("cluster scan cursor should be serializable")
    )
}

/// 各类型的元素计数命令。与前端 `lib/keyTypes.ts` 的 `sizeCommand` 一一对应。
fn size_command(key_type: &str) -> Option<&'static str> {
    match key_type {
        "string" => Some("STRLEN"),
        "hash" => Some("HLEN"),
        "list" => Some("LLEN"),
        "set" => Some("SCARD"),
        "zset" => Some("ZCARD"),
        "stream" => Some("XLEN"),
        _ => None,
    }
}

pub async fn get_key_meta(state: &AppState, connection_id: &str, key: &str) -> AppResult<KeyMeta> {
    let runtime = session_runtime(state, connection_id).await?;
    let mut executor = runtime.executor.lock().await;
    let cluster = executor.is_cluster();

    // 第一批：TYPE 决定了下一批用哪个大小命令，所以它必须先落地。
    let mut probe = redis::pipe();
    probe.cmd("TYPE").arg(key);
    probe.cmd("TTL").arg(key);
    let probe = executor
        .query_pipeline_raw(probe, PipelineTarget::Key(key))
        .await?;

    let ttl = redis_values::optional_value::<i64>(probe.get(1)).unwrap_or(-2);
    let key_type =
        redis_values::optional_value::<String>(probe.first()).filter(|key_type| key_type != "none");

    // key 不在了就到此为止：后面几条对着一个不存在的 key 发没有意义。
    let Some(key_type) = key_type else {
        return Ok(KeyMeta {
            key: key.into(),
            key_type: None,
            ttl: -2,
            size: None,
            encoding: None,
            memory: None,
            slot: None,
            hll: None,
        });
    };

    // 第二批：大小 / 编码 / 内存 / 槽位。MEMORY 常被只读 ACL 或 rename-command
    // 挡掉，executor 开了 ignore_errors，所以坏的那一格变成 None 而不是整批失败。
    let mut detail = redis::pipe();
    let has_size = match size_command(&key_type) {
        Some(command) => {
            detail.cmd(command).arg(key);
            true
        }
        None => false,
    };
    detail.cmd("OBJECT").arg("ENCODING").arg(key);
    detail.cmd("MEMORY").arg("USAGE").arg(key);
    if cluster {
        detail.cmd("CLUSTER").arg("KEYSLOT").arg(key);
    }
    // HLL 是 string 的子形态：TYPE 认不出来，识别靠头部 16 字节的 HYLL 魔数。
    // 探针刻意用 GETRANGE 而不是 PFCOUNT —— 后者技术上是写命令（回写基数缓存），
    // 元信息读取在有纯读替代时没有理由留下服务端痕迹。SCAN 路径不做这个探测。
    let probe_hll = key_type == "string";
    if probe_hll {
        detail.cmd("GETRANGE").arg(key).arg(0).arg(15);
    }
    let detail = executor
        .query_pipeline_raw(detail, PipelineTarget::Key(key))
        .await?;

    let mut cell = if has_size { 1 } else { 0 };
    let size = if has_size {
        redis_values::optional_value::<u64>(detail.first())
    } else {
        None
    };
    let encoding = redis_values::optional_value::<String>(detail.get(cell));
    cell += 1;
    let memory = redis_values::optional_value::<u64>(detail.get(cell));
    cell += 1;
    let slot = if cluster {
        let value = redis_values::optional_value::<u16>(detail.get(cell));
        cell += 1;
        value
    } else {
        None
    };
    let hll = if probe_hll {
        redis_values::optional_bytes(detail.get(cell)).and_then(|bytes| hll::detect(&bytes))
    } else {
        None
    };

    Ok(KeyMeta {
        key: key.into(),
        key_type: Some(key_type),
        ttl,
        size,
        encoding,
        memory,
        slot,
        hll,
    })
}

pub struct DedicatedRedisClient {
    pub client: redis::Client,
    pub resources: Vec<SessionResource>,
}

const DEDICATED_TLS_SERVERNAME_OVERRIDE_UNSUPPORTED: &str =
    "Dedicated streaming tasks do not support TLS servername overrides";

pub async fn open_dedicated_client(config: &ConnectionConfig) -> AppResult<DedicatedRedisClient> {
    reject_dedicated_tls_servername_override(config)?;

    let plan = build_connection_plan(config)?;
    if matches!(
        plan.mode,
        ConnectionMode::Cluster
            | ConnectionMode::ClusterTls
            | ConnectionMode::ClusterSsh
            | ConnectionMode::ClusterTlsOverSsh
    ) {
        return Err(AppError::TaskInvalidRequest(
            "Dedicated streaming tasks are not supported for Redis Cluster sessions".into(),
        ));
    }
    reject_dedicated_tls_servername_for_target_rewrite(config, &plan.mode)?;

    let mut resources = Vec::new();
    let mut effective_config = config.clone();
    if uses_seed_ssh_tunnel(&plan.mode) {
        let ssh_options = require_ssh_options(config)?;
        let tunnel = ssh::open_tunnel(
            ssh_options,
            ssh::SshTarget {
                host: config.host.clone(),
                port: config.port,
            },
        )
        .await?;
        effective_config.host = tunnel.local_host.clone();
        effective_config.port = tunnel.local_port;
        resources.push(SessionResource::SshTunnel(tunnel));
    }

    if is_sentinel_mode(&plan.mode) {
        let sentinel_options = effective_config.sentinel_options.clone().ok_or_else(|| {
            AppError::ConnectionConfigInvalid("Sentinel options are required".into())
        })?;
        let mut sentinel_connection = open_sentinel_connection(&effective_config).await?;
        let master = match &mut sentinel_connection {
            SentinelConnection::Manager(manager) => {
                sentinel::resolve_master(manager, &sentinel_options.master_name).await?
            }
            SentinelConnection::Multiplexed(connection) => {
                sentinel::resolve_master(connection, &sentinel_options.master_name).await?
            }
        };
        if uses_resolved_sentinel_master_ssh_tunnel(&plan.mode) {
            let ssh_options = require_ssh_options(config)?;
            let tunnel = ssh::open_tunnel(
                ssh_options,
                ssh::SshTarget {
                    host: master.host,
                    port: master.port,
                },
            )
            .await?;
            effective_config.host = tunnel.local_host.clone();
            effective_config.port = tunnel.local_port;
            resources.push(SessionResource::SshTunnel(tunnel));
        } else {
            effective_config.host = master.host;
            effective_config.port = master.port;
        }
        if let Some(node_password) = non_empty_after_trim(sentinel_options.node_password.as_deref())
        {
            effective_config.auth = Some(node_password.to_string());
        }
    }

    reject_dedicated_tls_servername_override(&effective_config)?;

    let info = build_connection_info(&effective_config)?;
    let client =
        redis::Client::open(info).map_err(|e| AppError::RedisConnectionFailed(e.to_string()))?;
    Ok(DedicatedRedisClient { client, resources })
}

fn reject_dedicated_tls_servername_override(config: &ConnectionConfig) -> AppResult<()> {
    if tls_servername_resolver(config).is_some() {
        return Err(AppError::TaskInvalidRequest(
            DEDICATED_TLS_SERVERNAME_OVERRIDE_UNSUPPORTED.into(),
        ));
    }

    Ok(())
}

fn reject_dedicated_tls_servername_for_target_rewrite(
    config: &ConnectionConfig,
    mode: &ConnectionMode,
) -> AppResult<()> {
    let has_servername = config
        .ssl_options
        .as_ref()
        .and_then(|options| non_empty_after_trim(options.servername.as_deref()))
        .is_some();

    if has_servername && rewrites_dedicated_redis_target(mode) {
        return Err(AppError::TaskInvalidRequest(
            DEDICATED_TLS_SERVERNAME_OVERRIDE_UNSUPPORTED.into(),
        ));
    }

    Ok(())
}

fn rewrites_dedicated_redis_target(mode: &ConnectionMode) -> bool {
    matches!(
        mode,
        ConnectionMode::StandaloneTlsOverSsh
            | ConnectionMode::SentinelTls
            | ConnectionMode::SentinelTlsOverSsh
    )
}

/// 单条命令的响应超时。
///
/// redis-rs 的默认值是 **500ms**，对这个应用来说明显太紧：
///   - 跨 WAN 连一个远程实例，RTT 180ms 就吃掉预算的三分之一，抖动一下就超时
///   - 取值命令最多会搬 `DEFAULT_VALUE_LIMIT`（256 KiB）回来，
///     在任何有实际延迟的链路上都不可能在 500ms 内传完
///
/// 症状是随机的 "timed out"，而且越是大 key 越必现 —— 用户会以为是自己的
/// Redis 有问题。这是在 180ms 链路上跑 e2e 时撞出来的。
pub(crate) const REDIS_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// 建连超时。默认 1s 同样偏紧：TLS 握手加上跨洋 RTT 很容易超。
/// 比响应超时短是有意的 —— 连不上要快速失败，连上之后要有耐心。
pub(crate) const REDIS_CONNECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 建连重试次数。**默认的 6 次对交互式操作是灾难。**
///
/// redis-rs 把 `AuthenticationFailed` 也当作可重试错误，于是密码填错要退避重试
/// 满 6 轮才报错 —— 实测 9.6 秒，而密码错重试多少次都不可能成功。
/// 用户点了"连接"之后干等十秒没有任何反馈，只会以为是自己网络有问题。
///
/// 降到 1 次：同样的场景实测 176ms，同时保留一次重连机会，覆盖
/// "空闲一段时间后连接被服务端或 NAT 悄悄掐掉、下一条命令重连一次即可"——
/// 那是 `ConnectionManager` 真正值钱的地方。0 次会把这个场景也变成报错。
const REDIS_CONNECTION_RETRIES: usize = 1;

async fn build_standalone_executor(config: &ConnectionConfig) -> AppResult<RedisExecutor> {
    build_standalone_executor_with_timeout(config, REDIS_RESPONSE_TIMEOUT).await
}

async fn build_standalone_executor_with_timeout(
    config: &ConnectionConfig,
    response_timeout: std::time::Duration,
) -> AppResult<RedisExecutor> {
    let connection_info = build_connection_info(config)?;
    let client = redis::Client::open(connection_info)
        .map_err(|e| AppError::RedisConnectionFailed(e.to_string()))?;

    if let Some(resolver) = tls_servername_resolver(config) {
        let async_config = redis::AsyncConnectionConfig::new()
            .set_dns_resolver(resolver)
            .set_response_timeout(Some(response_timeout))
            .set_connection_timeout(Some(REDIS_CONNECTION_TIMEOUT));
        let connection = client
            .get_multiplexed_async_connection_with_config(&async_config)
            .await
            .map_err(|e| AppError::RedisConnectionFailed(e.to_string()))?;
        return Ok(RedisExecutor::StandaloneTls(connection));
    }

    let manager_config = redis::aio::ConnectionManagerConfig::new()
        .set_response_timeout(Some(response_timeout))
        .set_connection_timeout(Some(REDIS_CONNECTION_TIMEOUT))
        .set_number_of_retries(REDIS_CONNECTION_RETRIES);
    let manager = redis::aio::ConnectionManager::new_with_config(client, manager_config)
        .await
        .map_err(|e| AppError::RedisConnectionFailed(e.to_string()))?;
    Ok(RedisExecutor::Standalone(manager))
}

fn build_cluster_seed_url(config: &ConnectionConfig) -> String {
    build_cluster_node_url(&config.host, config.port, config.ssl_options.is_some())
}

fn build_cluster_ssh_seed_urls(
    masters: &[cluster::ClusterNode],
    config: &ConnectionConfig,
) -> Vec<String> {
    masters
        .iter()
        .map(|node| build_cluster_node_url(&node.host, node.port, config.ssl_options.is_some()))
        .collect()
}

fn build_cluster_node_url(host: &str, port: u16, tls: bool) -> String {
    let scheme = if tls { "rediss" } else { "redis" };
    format!("{}://{}:{}/", scheme, cluster_url_host(host), port)
}

fn cluster_url_host(host: &str) -> String {
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

async fn ping_executor(executor: &mut RedisExecutor) -> AppResult<()> {
    let command = redis::cmd("PING");
    match executor {
        RedisExecutor::Standalone(manager) => command
            .query_async::<redis::Value>(manager)
            .await
            .map_err(|e| AppError::RedisConnectionFailed(e.to_string()))?,
        RedisExecutor::StandaloneTls(connection) => command
            .query_async::<redis::Value>(connection)
            .await
            .map_err(|e| AppError::RedisConnectionFailed(e.to_string()))?,
        RedisExecutor::Cluster(connection) => command
            .query_async::<redis::Value>(connection)
            .await
            .map_err(|e| AppError::RedisConnectionFailed(e.to_string()))?,
    };
    Ok(())
}

fn build_connection_info(config: &ConnectionConfig) -> AppResult<redis::ConnectionInfo> {
    let mut redis_settings = RedisConnectionInfo::default().set_db(config.db.unwrap_or(0));
    if let Some(username) = non_empty_after_trim(config.username.as_deref()) {
        redis_settings = redis_settings.set_username(username);
    }
    if let Some(password) = non_empty_after_trim(config.auth.as_deref()) {
        redis_settings = redis_settings.set_password(password);
    }

    let info = (config.host.clone(), config.port)
        .into_connection_info()
        .map_err(|e| AppError::RedisConnectionFailed(e.to_string()))?
        .set_redis_settings(redis_settings);

    let Some(ssl_options) = &config.ssl_options else {
        return Ok(info);
    };
    tls::ensure_crypto_provider();
    let tls_host =
        non_empty_after_trim(ssl_options.servername.as_deref()).unwrap_or(config.host.as_str());

    let mut tls_addr = redis::ConnectionAddr::TcpTls {
        host: tls_host.to_string(),
        port: config.port,
        insecure: false,
        tls_params: None,
    };
    if ssl_options.insecure.unwrap_or(false) {
        tls_addr.set_danger_accept_invalid_hostnames(true);
    }
    let tls_info = info.set_addr(tls_addr);
    let Some(certs) = tls::read_tls_certificates(ssl_options)? else {
        return Ok(tls_info);
    };

    redis::Client::build_with_tls(tls_info, certs)
        .map(|client| client.get_connection_info().clone())
        .map(|info| apply_tls_insecure_hostname_validation(info, ssl_options))
        .map_err(|e| AppError::RedisTlsFailed(e.to_string()))
}

fn apply_tls_insecure_hostname_validation(
    connection_info: redis::ConnectionInfo,
    ssl_options: &crate::models::SslOptions,
) -> redis::ConnectionInfo {
    if !ssl_options.insecure.unwrap_or(false) {
        return connection_info;
    }

    let mut addr = connection_info.addr().clone();
    addr.set_danger_accept_invalid_hostnames(true);
    connection_info.set_addr(addr)
}

fn tls_servername_resolver(config: &ConnectionConfig) -> Option<TlsServerNameResolver> {
    let ssl_options = config.ssl_options.as_ref()?;
    let servername = non_empty_after_trim(ssl_options.servername.as_deref())?;
    if servername == config.host {
        return None;
    }

    Some(TlsServerNameResolver {
        servername: servername.to_string(),
        servername_port: config.port,
        target_host: config.host.clone(),
        target_port: config.port,
    })
}

#[derive(Clone, Debug)]
struct TlsServerNameResolver {
    servername: String,
    servername_port: u16,
    target_host: String,
    target_port: u16,
}

impl redis::io::AsyncDNSResolver for TlsServerNameResolver {
    fn resolve<'a, 'b: 'a>(
        &'a self,
        host: &'b str,
        port: u16,
    ) -> redis::RedisFuture<'a, Box<dyn Iterator<Item = SocketAddr> + Send + 'a>> {
        let (lookup_host, lookup_port) = if host == self.servername && port == self.servername_port
        {
            (self.target_host.as_str(), self.target_port)
        } else {
            (host, port)
        };

        Box::pin(async move {
            if let Ok(ip) = lookup_host.parse::<IpAddr>() {
                return Ok(Box::new(std::iter::once(SocketAddr::new(ip, lookup_port)))
                    as Box<dyn Iterator<Item = SocketAddr> + Send>);
            }

            let socket_addrs: Vec<_> = tokio::net::lookup_host((lookup_host, lookup_port))
                .await
                .map_err(redis::RedisError::from)?
                .collect();

            if socket_addrs.is_empty() {
                return Err(redis::RedisError::from((
                    redis::ErrorKind::InvalidClientConfig,
                    "No address found for host",
                )));
            }

            Ok(Box::new(socket_addrs.into_iter()) as Box<dyn Iterator<Item = SocketAddr> + Send>)
        })
    }
}

fn non_empty_after_trim(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

pub(crate) fn parse_scan_response(value: redis::Value) -> AppResult<(String, Vec<String>)> {
    let (cursor, keys): (String, Vec<Vec<u8>>) = FromRedisValue::from_redis_value(value)
        .map_err(|e| AppError::RedisCommandFailed(e.to_string()))?;
    let keys = keys
        .into_iter()
        .map(|key| {
            String::from_utf8(key).map_err(|_| {
                AppError::RedisCommandFailed("Phase 1 only supports UTF-8 Redis keys".into())
            })
        })
        .collect::<AppResult<Vec<_>>>()?;

    Ok((cursor, keys))
}

#[cfg(test)]
mod tests {
    use super::{
        build_cluster_seed_url, build_cluster_ssh_seed_urls, build_connection_info,
        decode_cluster_scan_cursor, encode_cluster_scan_cursor, is_supported_connection_mode,
        open_dedicated_client, parse_scan_response, reject_dedicated_tls_servername_override,
        sentinel_connection_strategy, tls_servername_resolver,
        uses_resolved_sentinel_master_ssh_tunnel, ConnectionMode, SentinelConnectionStrategy,
    };
    use crate::models::{ConnectionConfig, SentinelOptions, SshOptions, SslOptions};
    use crate::services::cluster::{self, ClusterNode};
    use redis::io::AsyncDNSResolver;
    use redis::ConnectionAddr;
    use std::collections::HashMap;

    fn config() -> ConnectionConfig {
        ConnectionConfig {
            host: "127.0.0.1".into(),
            port: 6379,
            auth: None,
            username: None,
            db: None,
            name: None,
            separator: None,
            connection_read_only: None,
            cluster: None,
            ssl_options: None,
            ssh_options: None,
            sentinel_options: None,
        }
    }

    #[test]
    fn trims_empty_username_and_auth_from_connection_settings() {
        let mut config = config();
        config.auth = Some("  ".into());
        config.username = Some("".into());

        let info = build_connection_info(&config).unwrap();

        assert_eq!(info.redis_settings().username(), None);
        assert_eq!(info.redis_settings().password(), None);
        assert_eq!(info.redis_settings().db(), 0);
    }

    #[test]
    fn preserves_non_empty_username_password_whitespace_and_sets_db() {
        let mut config = config();
        config.auth = Some(" secret:with@chars ".into());
        config.username = Some(" default ".into());
        config.db = Some(2);

        let info = build_connection_info(&config).unwrap();

        assert_eq!(info.redis_settings().username(), Some(" default "));
        assert_eq!(
            info.redis_settings().password(),
            Some(" secret:with@chars ")
        );
        assert_eq!(info.redis_settings().db(), 2);
    }

    #[test]
    fn keeps_ipv6_host_as_tcp_addr_without_url_brackets() {
        let mut config = config();
        config.host = "::1".into();

        let info = build_connection_info(&config).unwrap();

        assert!(matches!(
            info.addr(),
            ConnectionAddr::Tcp(host, 6379) if host == "::1"
        ));
    }

    #[test]
    fn builds_tls_connection_info_and_preserves_redis_settings() {
        let mut config = config();
        config.host = "redis.local".into();
        config.port = 6380;
        config.username = Some("default".into());
        config.auth = Some("secret".into());
        config.db = Some(4);
        config.ssl_options = Some(SslOptions {
            key: None,
            cert: None,
            ca: None,
            servername: None,
            insecure: None,
        });

        let info = build_connection_info(&config).unwrap();

        assert!(matches!(
            info.addr(),
            ConnectionAddr::TcpTls {
                host,
                port: 6380,
                insecure: false,
                tls_params: None,
            } if host == "redis.local"
        ));
        assert_eq!(info.redis_settings().username(), Some("default"));
        assert_eq!(info.redis_settings().password(), Some("secret"));
        assert_eq!(info.redis_settings().db(), 4);
    }

    #[test]
    fn builds_tls_connection_info_with_configured_servername_for_sni() {
        let mut config = config();
        config.host = "127.0.0.1".into();
        config.port = 6380;
        config.ssl_options = Some(SslOptions {
            key: None,
            cert: None,
            ca: None,
            servername: Some("redis-tls".into()),
            insecure: None,
        });

        let info = build_connection_info(&config).unwrap();

        assert!(matches!(
            info.addr(),
            ConnectionAddr::TcpTls {
                host,
                port: 6380,
                insecure: false,
                tls_params: None,
            } if host == "redis-tls"
        ));
    }

    #[test]
    fn rejects_dedicated_tls_servername_overrides_without_network() {
        let mut config = config();
        config.host = "127.0.0.1".into();
        config.port = 6380;
        config.ssl_options = Some(SslOptions {
            key: None,
            cert: None,
            ca: None,
            servername: Some("redis-tls".into()),
            insecure: None,
        });

        let payload = reject_dedicated_tls_servername_override(&config)
            .unwrap_err()
            .payload();

        assert_eq!(payload.code, "redis.task_invalid_request");
        assert_eq!(
            payload.detail.as_deref(),
            Some("Dedicated streaming tasks do not support TLS servername overrides")
        );
    }

    #[tokio::test]
    async fn rejects_dedicated_tls_servername_overrides_before_ssh_setup() {
        let mut config = config();
        config.host = "127.0.0.1".into();
        config.port = 6380;
        config.ssl_options = Some(SslOptions {
            key: None,
            cert: None,
            ca: None,
            servername: Some("redis-tls".into()),
            insecure: None,
        });
        config.ssh_options = Some(SshOptions {
            host: "127.0.0.1".into(),
            port: 1,
            username: "redis".into(),
            password: Some("secret".into()),
            privatekey: None,
            privatekeybookmark: None,
            passphrase: None,
            timeout: Some(1),
        });

        let payload = match open_dedicated_client(&config).await {
            Ok(_) => panic!("dedicated client should reject TLS servername overrides"),
            Err(err) => err.payload(),
        };

        assert_eq!(payload.code, "redis.task_invalid_request");
        assert_eq!(
            payload.detail.as_deref(),
            Some("Dedicated streaming tasks do not support TLS servername overrides")
        );
    }

    #[tokio::test]
    async fn rejects_dedicated_tls_servername_equal_to_host_before_target_rewrite() {
        let mut config = config();
        config.host = "redis.example.com".into();
        config.port = 6380;
        config.ssl_options = Some(SslOptions {
            key: None,
            cert: None,
            ca: None,
            servername: Some("redis.example.com".into()),
            insecure: None,
        });
        config.ssh_options = Some(SshOptions {
            host: "127.0.0.1".into(),
            port: 1,
            username: "redis".into(),
            password: Some("secret".into()),
            privatekey: None,
            privatekeybookmark: None,
            passphrase: None,
            timeout: Some(1),
        });

        let payload = match open_dedicated_client(&config).await {
            Ok(_) => panic!("dedicated client should reject TLS servername target rewrites"),
            Err(err) => err.payload(),
        };

        assert_eq!(payload.code, "redis.task_invalid_request");
        assert_eq!(
            payload.detail.as_deref(),
            Some("Dedicated streaming tasks do not support TLS servername overrides")
        );
    }

    #[test]
    fn tls_hostname_validation_can_be_explicitly_disabled() {
        let mut config = config();
        config.host = "redis.local".into();
        config.ssl_options = Some(SslOptions {
            key: None,
            cert: None,
            ca: None,
            servername: None,
            insecure: Some(true),
        });

        let info = build_connection_info(&config).unwrap();

        assert!(
            format!("{:?}", info.addr()).contains("danger_accept_invalid_hostnames: true"),
            "TLS hostname validation should be disabled when sslOptions.insecure is true"
        );
    }

    #[test]
    fn tls_hostname_validation_is_enabled_by_default() {
        let mut config = config();
        config.host = "redis.local".into();
        config.ssl_options = Some(SslOptions {
            key: None,
            cert: None,
            ca: None,
            servername: None,
            insecure: None,
        });

        let info = build_connection_info(&config).unwrap();

        assert!(
            !format!("{:?}", info.addr()).contains("danger_accept_invalid_hostnames: true"),
            "TLS hostname validation should not be disabled unless explicitly configured"
        );
    }

    #[tokio::test]
    async fn tls_servername_resolver_maps_sni_host_to_original_tcp_target() {
        let mut config = config();
        config.host = "127.0.0.1".into();
        config.port = 6380;
        config.ssl_options = Some(SslOptions {
            key: None,
            cert: None,
            ca: None,
            servername: Some("redis-tls".into()),
            insecure: None,
        });
        let resolver = tls_servername_resolver(&config).expect("resolver should be configured");

        let resolved = resolver
            .resolve("redis-tls", 6380)
            .await
            .unwrap()
            .collect::<Vec<_>>();

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].ip().to_string(), "127.0.0.1");
        assert_eq!(resolved[0].port(), 6380);
    }

    #[test]
    fn sentinel_tls_with_custom_servername_uses_dns_resolver_connection_strategy() {
        let mut config = config();
        config.host = "127.0.0.1".into();
        config.port = 26379;
        config.ssl_options = Some(SslOptions {
            key: None,
            cert: None,
            ca: None,
            servername: Some("redis-tls".into()),
            insecure: None,
        });
        config.sentinel_options = Some(SentinelOptions {
            master_name: "mymaster".into(),
            node_password: None,
        });

        assert_eq!(
            sentinel_connection_strategy(&config),
            SentinelConnectionStrategy::MultiplexedWithDnsResolver
        );
    }

    #[test]
    fn sentinel_ssh_modes_tunnel_resolved_master_for_dedicated_clients() {
        assert!(uses_resolved_sentinel_master_ssh_tunnel(
            &ConnectionMode::SentinelSsh
        ));
        assert!(uses_resolved_sentinel_master_ssh_tunnel(
            &ConnectionMode::SentinelTlsOverSsh
        ));
        assert!(!uses_resolved_sentinel_master_ssh_tunnel(
            &ConnectionMode::Sentinel
        ));
        assert!(!uses_resolved_sentinel_master_ssh_tunnel(
            &ConnectionMode::SentinelTls
        ));
    }

    #[test]
    fn rejects_non_utf8_scan_keys_with_clear_error() {
        let value = redis::Value::Array(vec![
            redis::Value::BulkString(b"0".to_vec()),
            redis::Value::Array(vec![redis::Value::BulkString(vec![0xff])]),
        ]);

        let err = parse_scan_response(value).unwrap_err();

        assert!(matches!(
            err,
            crate::error::AppError::RedisCommandFailed(detail)
                if detail == "Phase 1 only supports UTF-8 Redis keys"
        ));
    }

    #[test]
    fn cluster_scan_cursor_tracks_each_master_independently() {
        let masters = vec![
            ClusterNode {
                host: "10.0.0.2".into(),
                port: 7000,
            },
            ClusterNode {
                host: "10.0.0.3".into(),
                port: 7001,
            },
        ];

        let first_page = decode_cluster_scan_cursor("0", &masters).unwrap();

        assert_eq!(first_page.get("10.0.0.2:7000"), Some(&"0".to_string()));
        assert_eq!(first_page.get("10.0.0.3:7001"), Some(&"0".to_string()));

        let mut next_page = HashMap::new();
        next_page.insert("10.0.0.2:7000".to_string(), "12".to_string());
        let encoded = encode_cluster_scan_cursor(&next_page);
        let decoded = decode_cluster_scan_cursor(&encoded, &masters).unwrap();

        assert_eq!(decoded, next_page);
        assert_eq!(encode_cluster_scan_cursor(&HashMap::new()), "0");
    }

    #[test]
    fn builds_cluster_seed_urls_for_plain_tls_and_ipv6_hosts() {
        let mut config = config();
        assert_eq!(build_cluster_seed_url(&config), "redis://127.0.0.1:6379/");

        config.ssl_options = Some(SslOptions {
            key: None,
            cert: None,
            ca: None,
            servername: None,
            insecure: None,
        });
        assert_eq!(build_cluster_seed_url(&config), "rediss://127.0.0.1:6379/");

        config.host = "::1".into();
        assert_eq!(build_cluster_seed_url(&config), "rediss://[::1]:6379/");
    }

    #[tokio::test]
    async fn cluster_tls_over_ssh_seed_urls_use_real_master_hosts_even_with_custom_servername() {
        let mut config = config();
        config.ssl_options = Some(SslOptions {
            key: None,
            cert: None,
            ca: None,
            servername: Some("cluster.example.test".into()),
            insecure: None,
        });
        let masters = vec![
            ClusterNode {
                host: "10.0.0.2".into(),
                port: 7000,
            },
            ClusterNode {
                host: "10.0.0.3".into(),
                port: 7001,
            },
        ];
        let tunnels = vec![
            ClusterNode {
                host: "127.0.0.1".into(),
                port: 49152,
            },
            ClusterNode {
                host: "127.0.0.1".into(),
                port: 49153,
            },
        ];

        let seed_urls = build_cluster_ssh_seed_urls(&masters, &config);
        let resolver = cluster::build_tunnel_resolver(&masters, &tunnels).unwrap();
        let resolved = resolver
            .resolve("10.0.0.2", 7000)
            .await
            .unwrap()
            .collect::<Vec<_>>();

        assert_eq!(
            seed_urls,
            vec![
                "rediss://10.0.0.2:7000/".to_string(),
                "rediss://10.0.0.3:7001/".to_string()
            ]
        );
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].ip().to_string(), "127.0.0.1");
        assert_eq!(resolved[0].port(), 49152);
    }

    #[test]
    fn supports_cluster_and_sentinel_ssh_modes_after_task_8() {
        assert!(is_supported_connection_mode(
            &ConnectionMode::StandaloneTlsOverSsh
        ));
        assert!(is_supported_connection_mode(&ConnectionMode::ClusterSsh));
        assert!(is_supported_connection_mode(
            &ConnectionMode::ClusterTlsOverSsh
        ));
        assert!(is_supported_connection_mode(&ConnectionMode::SentinelSsh));
        assert!(is_supported_connection_mode(
            &ConnectionMode::SentinelTlsOverSsh
        ));
    }
}
