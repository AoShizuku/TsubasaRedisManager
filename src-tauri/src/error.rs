use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, Serialize)]
pub struct AppErrorPayload {
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
    pub recoverable: bool,
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{message}")]
    KeyWriteFailed { code: String, message: String },
    #[error("Connection configuration is invalid")]
    ConnectionConfigInvalid(String),
    #[error("Redis connection failed")]
    RedisConnectionFailed(String),
    #[error("TLS connection failed")]
    RedisTlsFailed(String),
    #[error("SSH tunnel failed")]
    RedisSshFailed(String),
    #[error("Cluster connection failed")]
    RedisClusterFailed(String),
    #[error("Sentinel connection failed")]
    RedisSentinelFailed(String),
    #[error("Redis command failed")]
    RedisCommandFailed(String),
    /// 服务端的 `NOPERM`。和「不支持」分开说，因为两者的处理人不同：
    /// 不支持是换服务端或升级，没权限是找管理员改 ACL。
    #[error("Permission denied")]
    PermissionDenied {
        /// 被拒的命令名（大写）。认不出时为 `None`，见 `AppError::classify`。
        command: Option<String>,
        detail: String,
    },
    /// 能力不足：建连探测判定不可用（`capabilities::refuse`），或运行时被服务端否定
    /// （`capabilities::degrade`）。文案由 `capabilities.rs` 生成。
    #[error("{message}")]
    CapabilityUnsupported {
        message: String,
        detail: Option<String>,
    },
    #[error("Connection not found")]
    ConnectionNotFound(String),
    #[error("Readonly connection rejected write command")]
    ReadonlyWriteRejected(String),
    #[error("Invalid command")]
    InvalidCommand(String),
    #[error("Native operation failed")]
    NativeFailed(String),
    #[error("Task not found")]
    TaskNotFound(String),
    #[error("Task was cancelled")]
    TaskCancelled(String),
    #[error("Task failed")]
    TaskFailed(String),
    #[error("Task request is invalid")]
    TaskInvalidRequest(String),
    #[error("{message}")]
    FormatterFailed {
        code: String,
        message: String,
        detail: Option<String>,
    },
    #[error("{message}")]
    CodecFailed {
        code: String,
        message: String,
        detail: Option<String>,
    },
    #[error("{message}")]
    ConnectionStoreFailed {
        code: String,
        message: String,
        detail: Option<String>,
    },
}

impl AppError {
    pub fn payload(&self) -> AppErrorPayload {
        match self {
            AppError::KeyWriteFailed { code, message } => AppErrorPayload {
                code: code.clone(),
                message: message.clone(),
                detail: None,
                recoverable: true,
            },
            AppError::ConnectionConfigInvalid(detail) => AppErrorPayload {
                code: "redis.connection_config_invalid".into(),
                message: "Connection configuration is invalid".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::RedisConnectionFailed(detail) => AppErrorPayload {
                code: "redis.connection_failed".into(),
                message: "Connection failed".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::RedisTlsFailed(detail) => AppErrorPayload {
                code: "redis.tls_failed".into(),
                message: "TLS connection failed".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::RedisSshFailed(detail) => AppErrorPayload {
                code: "redis.ssh_failed".into(),
                message: "SSH tunnel failed".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::RedisClusterFailed(detail) => AppErrorPayload {
                code: "redis.cluster_failed".into(),
                message: "Cluster connection failed".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::RedisSentinelFailed(detail) => AppErrorPayload {
                code: "redis.sentinel_failed".into(),
                message: "Sentinel connection failed".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::RedisCommandFailed(detail) => AppErrorPayload {
                code: "redis.command_failed".into(),
                message: "Redis command failed".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::PermissionDenied { command, detail } => AppErrorPayload {
                code: "redis.permission_denied".into(),
                message: match command {
                    Some(command) => format!("当前用户无权执行 `{command}`"),
                    None => "当前用户无权执行脚本里调用的命令".into(),
                },
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::CapabilityUnsupported { message, detail } => AppErrorPayload {
                code: "redis.capability_unsupported".into(),
                message: message.clone(),
                detail: detail.clone(),
                recoverable: true,
            },
            AppError::ConnectionNotFound(id) => AppErrorPayload {
                code: "redis.connection_not_found".into(),
                message: "Connection is closed".into(),
                detail: Some(id.clone()),
                recoverable: true,
            },
            AppError::ReadonlyWriteRejected(command) => AppErrorPayload {
                code: "redis.readonly_write_rejected".into(),
                message: "You are in readonly mode. Unable to execute write command.".into(),
                detail: Some(command.clone()),
                recoverable: true,
            },
            AppError::InvalidCommand(detail) => AppErrorPayload {
                code: "redis.invalid_command".into(),
                message: "Invalid command".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::NativeFailed(detail) => AppErrorPayload {
                code: "native.failed".into(),
                message: "Native operation failed".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::TaskNotFound(detail) => AppErrorPayload {
                code: "redis.task_not_found".into(),
                message: "Task was not found".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::TaskCancelled(detail) => AppErrorPayload {
                code: "redis.task_cancelled".into(),
                message: "Task was cancelled".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::TaskFailed(detail) => AppErrorPayload {
                code: "redis.task_failed".into(),
                message: "Task failed".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::TaskInvalidRequest(detail) => AppErrorPayload {
                code: "redis.task_invalid_request".into(),
                message: "Task request is invalid".into(),
                detail: Some(detail.clone()),
                recoverable: true,
            },
            AppError::FormatterFailed {
                code,
                message,
                detail,
            } => AppErrorPayload {
                code: code.clone(),
                message: message.clone(),
                detail: detail.clone(),
                recoverable: true,
            },
            AppError::CodecFailed {
                code,
                message,
                detail,
            } => AppErrorPayload {
                code: code.clone(),
                message: message.clone(),
                detail: detail.clone(),
                recoverable: true,
            },
            AppError::ConnectionStoreFailed {
                code,
                message,
                detail,
            } => AppErrorPayload {
                code: code.clone(),
                message: message.clone(),
                detail: detail.clone(),
                recoverable: true,
            },
        }
    }
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.payload().serialize(serializer)
    }
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    /// 一条命令的失败。权限拒绝单独成类，其余照旧是 `RedisCommandFailed`。
    pub fn from_redis(command: &str, error: &redis::RedisError) -> AppError {
        let noperm = error.kind() == redis::ErrorKind::Server(redis::ServerErrorKind::NoPerm);
        Self::classify(command, noperm, error.to_string())
    }

    /// pipeline 里坏掉的那一格。
    pub fn from_server_error(command: &str, error: &redis::ServerError) -> AppError {
        let noperm = error.kind() == Some(redis::ServerErrorKind::NoPerm);
        Self::classify(command, noperm, error.to_string())
    }

    /// 权限拒绝有三种形状（e2e 实测，`tests/redis_capabilities.rs`）：
    ///
    /// | 场景 | 回复 |
    /// |---|---|
    /// | 直接调用，6 / 7 / Valkey | `NOPERM … run the 'expire' command …` |
    /// | 脚本内，Redis 7 / Valkey | `ERR ACL failure in script: … run the 'set' command …` |
    /// | 脚本内，Redis 6 | `ERR Error running script … can't run this command or subcommand`（没有命令名） |
    ///
    /// 后两种错误码是 `ERR`，只能按正文认。
    fn classify(command: &str, noperm: bool, detail: String) -> AppError {
        let in_script = detail.contains("ACL failure in script")
            || detail.contains("can't run this command or subcommand");
        if !noperm && !in_script {
            return AppError::RedisCommandFailed(detail);
        }
        // 脚本里被拒的是脚本调用的那条，不是 EVAL —— 认不出来就不点名，免得把人引向 EVAL
        let named = denied_command(&detail).or((!in_script).then_some(command));
        AppError::PermissionDenied {
            command: named.map(str::to_ascii_uppercase),
            detail,
        }
    }
}

/// 从权限拒绝的正文里取被拒的命令名：各版本都写成 `run the '…' command`。
/// 访问 key / channel 被拒、Redis 6 的脚本内拒绝没有命令名，返回 `None`。
fn denied_command(detail: &str) -> Option<&str> {
    let rest = &detail[detail.find("run the '")? + "run the '".len()..];
    let name = &rest[..rest.find('\'')?];
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_formatter_error_payload_with_formatter_code() {
        let payload = AppError::FormatterFailed {
            code: "formatter.command_missing".into(),
            message: "Formatter command is missing".into(),
            detail: Some("command".into()),
        }
        .payload();

        assert_eq!(payload.code, "formatter.command_missing");
        assert_eq!(payload.message, "Formatter command is missing");
        assert_eq!(payload.detail.as_deref(), Some("command"));
        assert!(payload.recoverable);
    }

    #[test]
    fn serializes_codec_error_payload_with_codec_code() {
        let payload = AppError::CodecFailed {
            code: "codec.decode_failed".into(),
            message: "Codec decode failed".into(),
            detail: Some("gzip data invalid".into()),
        }
        .payload();

        assert_eq!(payload.code, "codec.decode_failed");
        assert_eq!(payload.message, "Codec decode failed");
        assert_eq!(payload.detail.as_deref(), Some("gzip data invalid"));
        assert!(payload.recoverable);
    }

    fn server_error(line: &str) -> redis::ServerError {
        match redis::parse_redis_value(format!("-{line}\r\n").as_bytes()).unwrap() {
            redis::Value::ServerError(error) => error,
            other => panic!("expected a server error, got {other:?}"),
        }
    }

    #[test]
    fn noperm_is_its_own_error_and_names_the_rejected_command() {
        let error = AppError::from_server_error(
            "EXPIRE",
            &server_error("NOPERM User viewer has no permissions to run the 'expire' command"),
        );
        let payload = error.payload();
        assert_eq!(payload.code, "redis.permission_denied");
        assert_eq!(payload.message, "当前用户无权执行 `EXPIRE`");

        // 以下样本取自真实服务端（redis:7-alpine、redis:6-alpine）。
        // 脚本里被拒的是脚本调用的那条命令，不是 EVAL；错误码是 ERR，只能按正文认
        let in_script = AppError::from_server_error(
            "EVAL",
            &server_error(
                "ERR ACL failure in script: User probe has no permissions to run the 'set' command script: 219bb33e7c6773035c720850383aa2ebb63d9904, on @user_script:1.",
            ),
        );
        assert_eq!(in_script.payload().message, "当前用户无权执行 `SET`");

        // Redis 6 的脚本内拒绝没有命令名：不点名，也不能错点成 EVAL
        let redis6 = AppError::from_server_error(
            "EVAL",
            &server_error(
                "ERR Error running script (call to f_219bb33e7c6773035c720850383aa2ebb63d9904): @user_script:1: @user_script: 1: The user executing the script can't run this command or subcommand",
            ),
        );
        assert!(matches!(
            redis6,
            AppError::PermissionDenied { command: None, .. }
        ));
        assert_eq!(redis6.payload().message, "当前用户无权执行脚本里调用的命令");

        // 被拒的是 key 而不是命令时，退回到发出去的那条
        let key_denied = AppError::from_server_error(
            "get",
            &server_error("NOPERM No permissions to access a key"),
        );
        assert_eq!(key_denied.payload().message, "当前用户无权执行 `GET`");
    }

    #[test]
    fn other_server_errors_stay_command_failures() {
        let error = AppError::from_server_error("SET", &server_error("ERR syntax error"));
        assert!(
            matches!(error, AppError::RedisCommandFailed(detail) if detail.contains("syntax error"))
        );
    }

    #[test]
    fn serializes_connection_store_error_payload_with_store_code() {
        let payload = AppError::ConnectionStoreFailed {
            code: "connection_store.secret_read_failed".into(),
            message: "Connection secret could not be read".into(),
            detail: Some("credential provider failed".into()),
        }
        .payload();

        assert_eq!(payload.code, "connection_store.secret_read_failed");
        assert_eq!(payload.message, "Connection secret could not be read");
        assert_eq!(
            payload.detail.as_deref(),
            Some("credential provider failed")
        );
        assert!(payload.recoverable);
    }
}
