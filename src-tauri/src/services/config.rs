use crate::error::{AppError, AppResult};
use crate::models::ConnectionConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionMode {
    Standalone,
    StandaloneTls,
    StandaloneSsh,
    StandaloneTlsOverSsh,
    Cluster,
    ClusterTls,
    ClusterSsh,
    ClusterTlsOverSsh,
    Sentinel,
    SentinelTls,
    SentinelSsh,
    SentinelTlsOverSsh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionPlan {
    pub mode: ConnectionMode,
    pub readonly: bool,
    pub connection_name: String,
}

pub fn build_connection_plan(config: &ConnectionConfig) -> AppResult<ConnectionPlan> {
    let cluster = config.cluster.unwrap_or(false);
    let sentinel = config.sentinel_options.is_some();
    let tls = config.ssl_options.is_some();
    let ssh = config.ssh_options.is_some();

    if cluster && sentinel {
        return Err(AppError::ConnectionConfigInvalid(
            "Cluster and Sentinel cannot be enabled together".into(),
        ));
    }

    if config.host.trim().is_empty() {
        return Err(AppError::ConnectionConfigInvalid(
            "Redis host is required".into(),
        ));
    }

    if let Some(sentinel_options) = &config.sentinel_options {
        if sentinel_options.master_name.trim().is_empty() {
            return Err(AppError::ConnectionConfigInvalid(
                "Sentinel master name is required".into(),
            ));
        }
    }

    if let Some(ssh_options) = &config.ssh_options {
        if ssh_options.host.trim().is_empty() || ssh_options.username.trim().is_empty() {
            return Err(AppError::ConnectionConfigInvalid(
                "SSH host and username are required".into(),
            ));
        }

        if is_blank(ssh_options.password.as_deref()) && is_blank(ssh_options.privatekey.as_deref())
        {
            return Err(AppError::ConnectionConfigInvalid(
                "SSH password or private key is required".into(),
            ));
        }
    }

    let mode = match (cluster, sentinel, tls, ssh) {
        (false, false, false, false) => ConnectionMode::Standalone,
        (false, false, true, false) => ConnectionMode::StandaloneTls,
        (false, false, false, true) => ConnectionMode::StandaloneSsh,
        (false, false, true, true) => ConnectionMode::StandaloneTlsOverSsh,
        (true, false, false, false) => ConnectionMode::Cluster,
        (true, false, true, false) => ConnectionMode::ClusterTls,
        (true, false, false, true) => ConnectionMode::ClusterSsh,
        (true, false, true, true) => ConnectionMode::ClusterTlsOverSsh,
        (false, true, false, false) => ConnectionMode::Sentinel,
        (false, true, true, false) => ConnectionMode::SentinelTls,
        (false, true, false, true) => ConnectionMode::SentinelSsh,
        (false, true, true, true) => ConnectionMode::SentinelTlsOverSsh,
        (true, true, _, _) => unreachable!("cluster and sentinel conflict is checked above"),
    };

    Ok(ConnectionPlan {
        mode,
        readonly: config.connection_read_only.unwrap_or(false),
        connection_name: config
            .name
            .as_ref()
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| {
                format!("{}:{}/{}", config.host, config.port, config.db.unwrap_or(0))
            }),
    })
}

fn is_blank(value: Option<&str>) -> bool {
    value.map(|value| value.trim().is_empty()).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::{build_connection_plan, ConnectionMode};
    use crate::models::{ConnectionConfig, SentinelOptions, SshOptions, SslOptions};

    fn base() -> ConnectionConfig {
        ConnectionConfig {
            host: "127.0.0.1".into(),
            port: 6379,
            auth: None,
            username: None,
            db: None,
            name: Some("local".into()),
            separator: Some(":".into()),
            connection_read_only: Some(false),
            cluster: None,
            ssl_options: None,
            ssh_options: None,
            sentinel_options: None,
        }
    }

    fn tls() -> SslOptions {
        SslOptions {
            key: Some("client.key".into()),
            cert: Some("client.crt".into()),
            ca: Some("ca.crt".into()),
            servername: Some("redis.local".into()),
            insecure: None,
        }
    }

    fn ssh() -> SshOptions {
        SshOptions {
            host: "ssh.local".into(),
            port: 22,
            username: "redis".into(),
            password: Some("secret".into()),
            privatekey: None,
            privatekeybookmark: None,
            passphrase: None,
            timeout: Some(30),
        }
    }

    #[test]
    fn detects_all_supported_modes() {
        let cases = [
            (base(), ConnectionMode::Standalone),
            (
                {
                    let mut c = base();
                    c.ssl_options = Some(tls());
                    c
                },
                ConnectionMode::StandaloneTls,
            ),
            (
                {
                    let mut c = base();
                    c.ssh_options = Some(ssh());
                    c
                },
                ConnectionMode::StandaloneSsh,
            ),
            (
                {
                    let mut c = base();
                    c.ssl_options = Some(tls());
                    c.ssh_options = Some(ssh());
                    c
                },
                ConnectionMode::StandaloneTlsOverSsh,
            ),
            (
                {
                    let mut c = base();
                    c.cluster = Some(true);
                    c
                },
                ConnectionMode::Cluster,
            ),
            (
                {
                    let mut c = base();
                    c.cluster = Some(true);
                    c.ssl_options = Some(tls());
                    c
                },
                ConnectionMode::ClusterTls,
            ),
            (
                {
                    let mut c = base();
                    c.cluster = Some(true);
                    c.ssh_options = Some(ssh());
                    c
                },
                ConnectionMode::ClusterSsh,
            ),
            (
                {
                    let mut c = base();
                    c.cluster = Some(true);
                    c.ssl_options = Some(tls());
                    c.ssh_options = Some(ssh());
                    c
                },
                ConnectionMode::ClusterTlsOverSsh,
            ),
            (
                {
                    let mut c = base();
                    c.sentinel_options = Some(SentinelOptions {
                        master_name: "mymaster".into(),
                        node_password: None,
                    });
                    c
                },
                ConnectionMode::Sentinel,
            ),
            (
                {
                    let mut c = base();
                    c.sentinel_options = Some(SentinelOptions {
                        master_name: "mymaster".into(),
                        node_password: None,
                    });
                    c.ssl_options = Some(tls());
                    c
                },
                ConnectionMode::SentinelTls,
            ),
            (
                {
                    let mut c = base();
                    c.sentinel_options = Some(SentinelOptions {
                        master_name: "mymaster".into(),
                        node_password: None,
                    });
                    c.ssh_options = Some(ssh());
                    c
                },
                ConnectionMode::SentinelSsh,
            ),
            (
                {
                    let mut c = base();
                    c.sentinel_options = Some(SentinelOptions {
                        master_name: "mymaster".into(),
                        node_password: None,
                    });
                    c.ssl_options = Some(tls());
                    c.ssh_options = Some(ssh());
                    c
                },
                ConnectionMode::SentinelTlsOverSsh,
            ),
        ];

        for (config, expected) in cases {
            let plan = build_connection_plan(&config).expect("plan should build");
            assert_eq!(plan.mode, expected);
        }
    }

    #[test]
    fn rejects_cluster_and_sentinel_together() {
        let mut config = base();
        config.cluster = Some(true);
        config.sentinel_options = Some(SentinelOptions {
            master_name: "mymaster".into(),
            node_password: None,
        });

        let err = build_connection_plan(&config).unwrap_err();

        assert_eq!(err.payload().code, "redis.connection_config_invalid");
    }

    #[test]
    fn rejects_ssh_without_auth_method() {
        let mut config = base();
        config.ssh_options = Some(SshOptions {
            password: None,
            privatekey: None,
            ..ssh()
        });

        let err = build_connection_plan(&config).unwrap_err();

        assert_eq!(err.payload().code, "redis.connection_config_invalid");
    }

    #[test]
    fn rejects_blank_redis_host() {
        let mut config = base();
        config.host = "  ".into();

        let err = build_connection_plan(&config).unwrap_err();

        assert_eq!(err.payload().code, "redis.connection_config_invalid");
        assert_eq!(
            err.payload().detail.as_deref(),
            Some("Redis host is required")
        );
    }

    #[test]
    fn rejects_blank_sentinel_master_name() {
        let mut config = base();
        config.sentinel_options = Some(SentinelOptions {
            master_name: " ".into(),
            node_password: None,
        });

        let err = build_connection_plan(&config).unwrap_err();

        assert_eq!(err.payload().code, "redis.connection_config_invalid");
        assert_eq!(
            err.payload().detail.as_deref(),
            Some("Sentinel master name is required")
        );
    }

    #[test]
    fn rejects_blank_ssh_host_or_username() {
        let mut config = base();
        config.ssh_options = Some(SshOptions {
            host: "".into(),
            username: " ".into(),
            ..ssh()
        });

        let err = build_connection_plan(&config).unwrap_err();

        assert_eq!(err.payload().code, "redis.connection_config_invalid");
        assert_eq!(
            err.payload().detail.as_deref(),
            Some("SSH host and username are required")
        );
    }

    #[test]
    fn sets_readonly_and_defaults_connection_name() {
        let mut config = base();
        config.name = Some(" ".into());
        config.db = Some(3);
        config.connection_read_only = Some(true);

        let plan = build_connection_plan(&config).expect("plan should build");

        assert!(plan.readonly);
        assert_eq!(plan.connection_name, "127.0.0.1:6379/3");
    }

    #[test]
    fn preserves_explicit_non_blank_connection_name() {
        let mut config = base();
        config.name = Some("Local Redis".into());

        let plan = build_connection_plan(&config).expect("plan should build");

        assert_eq!(plan.connection_name, "Local Redis");
    }
}
