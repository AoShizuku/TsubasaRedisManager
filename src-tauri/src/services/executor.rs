use crate::error::{AppError, AppResult};
use crate::models::RedisValue;
use crate::services::redis_values;
use redis::cluster_routing::{Route, RoutingInfo, SingleNodeRoutingInfo, SlotAddr};

pub enum RedisExecutor {
    Standalone(redis::aio::ConnectionManager),
    StandaloneTls(redis::aio::MultiplexedConnection),
    Cluster(redis::cluster_async::ClusterConnection),
}

/// Pipeline 的落点。
///
/// 集群下 redis-rs 只按第一条命令路由**整个** pipeline，跨槽位的批会打到错误的
/// 节点。所以调用方必须显式说明这批命令落在哪 —— 把危险的情况做成不可表达的，
/// 比在文档里写一句"注意不要跨槽位"可靠。单机下这个参数被忽略。
pub enum PipelineTarget<'a> {
    /// 整批命令都作用于同一个 key，按该 key 的槽位路由。
    Key(&'a str),
    /// 整批命令都落在同一个主节点持有的槽位上，按地址路由。
    /// 分节点 SCAN 时用它：那一页的 key 全部来自该节点。
    Node { host: String, port: u16 },
    /// 整批命令都不带 key、问的是服务端本身（建连时的 `INFO` + `COMMAND INFO`），
    /// 任一主节点都能答。集群只探测一个节点，见 doc/backend/capabilities.md「探测」。
    AnyNode,
}

/// 命令名，给 `AppError::from_redis` 兜底用。
pub(crate) fn command_name(command: &redis::Cmd) -> String {
    match command.args_iter().next() {
        Some(redis::Arg::Simple(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
        _ => String::new(),
    }
}

impl RedisExecutor {
    pub async fn query(&mut self, command: redis::Cmd) -> AppResult<RedisValue> {
        let value = self.query_raw(command).await?;
        Ok(redis_values::from_redis_value(value))
    }

    pub async fn query_raw(&mut self, command: redis::Cmd) -> AppResult<redis::Value> {
        let name = command_name(&command);
        self.query_raw_detailed(command)
            .await
            .map_err(|e| AppError::from_redis(&name, &e))
    }

    /// 写入任务需要区别服务器明确拒绝与通信失败（后者可能已提交）。
    pub(crate) async fn query_raw_detailed(
        &mut self,
        command: redis::Cmd,
    ) -> Result<redis::Value, redis::RedisError> {
        match self {
            RedisExecutor::Standalone(manager) => {
                command.query_async::<redis::Value>(manager).await
            }
            RedisExecutor::StandaloneTls(connection) => {
                command.query_async::<redis::Value>(connection).await
            }
            RedisExecutor::Cluster(connection) => {
                command.query_async::<redis::Value>(connection).await
            }
        }
    }

    /// 批量发一组命令，一次往返。
    ///
    /// 这是语义化取值命令的底座：打开一个 key 需要 TYPE / TTL / 大小 / ENCODING /
    /// MEMORY 五六条命令，v0.1 那样逐条串行发就是逐条 IPC + 逐条往返。
    /// 单条命令失败时返回的是 `Value::ServerError` 那一格，而不是整批失败 ——
    /// 这里统一开了 `ignore_errors`。`MEMORY USAGE` 可能被 ACL 或 `rename-command`
    /// 拒掉，不该因此让 TYPE / TTL 也拿不到。调用方用 `optional_value` 读每一格。
    pub async fn query_pipeline_raw(
        &mut self,
        mut pipeline: redis::Pipeline,
        target: PipelineTarget<'_>,
    ) -> AppResult<Vec<redis::Value>> {
        pipeline.ignore_errors();
        let pipeline = &pipeline;
        match self {
            RedisExecutor::Standalone(manager) => pipeline
                .query_async::<Vec<redis::Value>>(manager)
                .await
                .map_err(|e| AppError::RedisCommandFailed(e.to_string())),
            RedisExecutor::StandaloneTls(connection) => pipeline
                .query_async::<Vec<redis::Value>>(connection)
                .await
                .map_err(|e| AppError::RedisCommandFailed(e.to_string())),
            RedisExecutor::Cluster(connection) => {
                let routing = match target {
                    PipelineTarget::Key(key) => {
                        SingleNodeRoutingInfo::SpecificNode(Route::with_key(key, SlotAddr::Master))
                    }
                    PipelineTarget::Node { host, port } => {
                        SingleNodeRoutingInfo::ByAddress { host, port }
                    }
                    PipelineTarget::AnyNode => SingleNodeRoutingInfo::RandomPrimary,
                };
                // 非事务 pipeline：从第 0 条起取满 len() 个回复。
                connection
                    .route_pipeline(pipeline.clone(), 0, pipeline.len(), routing)
                    .await
                    .map_err(|e| AppError::RedisCommandFailed(e.to_string()))
            }
        }
    }

    pub fn is_cluster(&self) -> bool {
        matches!(self, RedisExecutor::Cluster(_))
    }

    pub async fn query_cluster_node_raw(
        &mut self,
        command: redis::Cmd,
        host: String,
        port: u16,
    ) -> AppResult<redis::Value> {
        let RedisExecutor::Cluster(connection) = self else {
            return self.query_raw(command).await;
        };

        let name = command_name(&command);
        connection
            .route_command(
                command,
                RoutingInfo::SingleNode(SingleNodeRoutingInfo::ByAddress { host, port }),
            )
            .await
            .map_err(|e| AppError::from_redis(&name, &e))
    }
}
