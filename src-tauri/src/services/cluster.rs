use std::collections::HashMap;
use std::net::SocketAddr;

use crate::error::{AppError, AppResult};
use crate::models::ConnectionConfig;
use crate::services::tls;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterNode {
    pub host: String,
    pub port: u16,
}

pub fn parse_master_nodes(nodes: &str) -> AppResult<Vec<ClusterNode>> {
    nodes
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let columns = line.split_whitespace().collect::<Vec<_>>();
            if columns.len() < 8 || !is_healthy_master(columns[2], columns[7]) {
                return None;
            }

            Some(parse_cluster_address(columns[1]))
        })
        .collect()
}

pub fn tunnel_targets_for_masters(nodes: &str) -> AppResult<Vec<ClusterNode>> {
    parse_master_nodes(nodes)
}

pub fn build_nat_map(
    nodes: &[ClusterNode],
    tunnels: &[ClusterNode],
) -> AppResult<Vec<(String, String)>> {
    if nodes.len() != tunnels.len() {
        return Err(AppError::RedisClusterFailed(
            "Cluster NAT map requires matching node and tunnel counts".into(),
        ));
    }

    Ok(nodes
        .iter()
        .zip(tunnels)
        .map(|(node, tunnel)| (node.endpoint(), tunnel.endpoint()))
        .collect())
}

pub(crate) fn build_tunnel_resolver(
    nodes: &[ClusterNode],
    tunnels: &[ClusterNode],
) -> AppResult<ClusterTunnelResolver> {
    if nodes.len() != tunnels.len() {
        return Err(AppError::RedisClusterFailed(
            "Cluster NAT map requires matching node and tunnel counts".into(),
        ));
    }

    Ok(ClusterTunnelResolver::new(
        nodes.iter().cloned().zip(tunnels.iter().cloned()),
    ))
}

#[derive(Clone, Debug)]
pub(crate) struct ClusterTunnelResolver {
    nat_map: HashMap<(String, u16), ClusterNode>,
}

impl ClusterTunnelResolver {
    fn new(pairs: impl IntoIterator<Item = (ClusterNode, ClusterNode)>) -> Self {
        let nat_map = pairs
            .into_iter()
            .map(|(node, tunnel)| (resolver_key(&node.host, node.port), tunnel))
            .collect();
        Self { nat_map }
    }
}

impl redis::io::AsyncDNSResolver for ClusterTunnelResolver {
    fn resolve<'a, 'b: 'a>(
        &'a self,
        host: &'b str,
        port: u16,
    ) -> redis::RedisFuture<'a, Box<dyn Iterator<Item = SocketAddr> + Send + 'a>> {
        let (lookup_host, lookup_port) = self
            .nat_map
            .get(&resolver_key(host, port))
            .map(|target| (target.host.clone(), target.port))
            .unwrap_or_else(|| (host.to_string(), port));

        Box::pin(async move {
            let socket_addrs: Vec<_> = tokio::net::lookup_host((lookup_host.as_str(), lookup_port))
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

fn resolver_key(host: &str, port: u16) -> (String, u16) {
    (host.to_ascii_lowercase(), port)
}

pub async fn open_cluster_connection(
    nodes: Vec<String>,
    config: &ConnectionConfig,
) -> AppResult<redis::cluster_async::ClusterConnection> {
    open_cluster_connection_with_resolver(nodes, config, None).await
}

pub(crate) async fn open_cluster_connection_with_resolver(
    nodes: Vec<String>,
    config: &ConnectionConfig,
    resolver: Option<ClusterTunnelResolver>,
) -> AppResult<redis::cluster_async::ClusterConnection> {
    open_cluster_connection_with_timeout(
        nodes,
        config,
        resolver,
        crate::services::connection::REDIS_RESPONSE_TIMEOUT,
    )
    .await
}

pub(crate) async fn open_cluster_connection_with_timeout(
    nodes: Vec<String>,
    config: &ConnectionConfig,
    resolver: Option<ClusterTunnelResolver>,
    response_timeout: std::time::Duration,
) -> AppResult<redis::cluster_async::ClusterConnection> {
    let mut builder = redis::cluster::ClusterClientBuilder::new(nodes)
        // 与单机侧保持一致，理由见 connection::REDIS_RESPONSE_TIMEOUT。
        // 集群的默认值两头都不合适：建连超时同样是 1s（跨 WAN 偏紧），
        // 而响应超时默认是 None —— 一个卡住的节点会让界面永远等下去。
        .response_timeout(response_timeout)
        .connection_timeout(crate::services::connection::REDIS_CONNECTION_TIMEOUT);

    if let Some(username) = non_empty_after_trim(config.username.as_deref()) {
        builder = builder.username(username);
    }
    if let Some(password) = non_empty_after_trim(config.auth.as_deref()) {
        builder = builder.password(password);
    }

    if let Some(ssl_options) = &config.ssl_options {
        builder = builder.tls(redis::TlsMode::Secure);
        if ssl_options.insecure.unwrap_or(false) {
            builder = builder.danger_accept_invalid_hostnames(true);
        }

        if let Some(certs) = tls::read_tls_certificates(ssl_options).map_err(cluster_error)? {
            builder = builder.certs(certs);
        }
    }

    if let Some(resolver) = resolver {
        builder = builder.async_dns_resolver(resolver);
    }

    let client = builder.build().map_err(|err| {
        AppError::RedisClusterFailed(format!("Failed to build Redis Cluster client: {err}"))
    })?;
    client
        .get_async_connection()
        .await
        .map_err(|err| AppError::RedisClusterFailed(err.to_string()))
}

impl ClusterNode {
    pub(crate) fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

fn is_healthy_master(flags: &str, link_state: &str) -> bool {
    let flags = flags.split(',').collect::<Vec<_>>();
    flags.contains(&"master")
        && !flags.iter().any(|flag| *flag == "fail" || *flag == "fail?")
        && link_state == "connected"
}

fn parse_cluster_address(address: &str) -> AppResult<ClusterNode> {
    let address = address.split(',').next().unwrap_or(address);
    let address = address.split('@').next().unwrap_or(address);

    if let Some(rest) = address.strip_prefix('[') {
        let (host, port) = rest.split_once("]:").ok_or_else(|| {
            AppError::RedisClusterFailed(format!("Invalid Redis Cluster node address: {address}"))
        })?;
        return Ok(ClusterNode {
            host: host.to_string(),
            port: parse_port(port, address)?,
        });
    }

    let (host, port) = address.rsplit_once(':').ok_or_else(|| {
        AppError::RedisClusterFailed(format!("Invalid Redis Cluster node address: {address}"))
    })?;
    Ok(ClusterNode {
        host: host.to_string(),
        port: parse_port(port, address)?,
    })
}

fn parse_port(port: &str, address: &str) -> AppResult<u16> {
    port.parse::<u16>().map_err(|_| {
        AppError::RedisClusterFailed(format!("Invalid Redis Cluster node address: {address}"))
    })
}

fn non_empty_after_trim(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn cluster_error(err: AppError) -> AppError {
    AppError::RedisClusterFailed(
        err.payload()
            .detail
            .unwrap_or_else(|| "Redis Cluster connection failed".into()),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        build_nat_map, build_tunnel_resolver, parse_master_nodes, tunnel_targets_for_masters,
        ClusterNode,
    };
    use redis::io::AsyncDNSResolver;

    #[test]
    fn parses_healthy_master_nodes_and_ipv6_cluster_addresses() {
        let nodes = "\
07c37dfeb2352e0b6e9 10.0.0.2:7000@17000 master - 0 0 1 connected 0-5460
2c2f17d6a9b7f3c2a9a 10.0.0.3:7001@17001 slave 07c37dfeb2352e0b6e9 0 0 2 connected
badnode 10.0.0.4:7002@17002 master,fail - 0 0 3 disconnected
3c3f17d6a9b7f3c2a9b [fd00::1]:7003@17003 master - 0 0 4 connected 5461-10922
";

        let masters = parse_master_nodes(nodes).unwrap();

        assert_eq!(
            masters,
            vec![
                ClusterNode {
                    host: "10.0.0.2".into(),
                    port: 7000,
                },
                ClusterNode {
                    host: "fd00::1".into(),
                    port: 7003,
                },
            ]
        );
    }

    #[test]
    fn excludes_pfailing_master_nodes_even_when_link_is_connected() {
        let nodes = "\
pfailnode 10.0.0.5:7005@17005 master,fail? - 0 0 5 connected 0-5460
goodnode 10.0.0.6:7006@17006 master - 0 0 6 connected 5461-10922
";

        let masters = parse_master_nodes(nodes).unwrap();

        assert_eq!(
            masters,
            vec![ClusterNode {
                host: "10.0.0.6".into(),
                port: 7006,
            }]
        );
    }

    #[test]
    fn builds_nat_map_pairs_from_real_nodes_to_local_tunnels() {
        let nodes = vec![ClusterNode {
            host: "10.0.0.2".into(),
            port: 7000,
        }];
        let tunnels = vec![ClusterNode {
            host: "127.0.0.1".into(),
            port: 49152,
        }];

        let nat_map = build_nat_map(&nodes, &tunnels).unwrap();

        assert_eq!(
            nat_map,
            vec![("10.0.0.2:7000".into(), "127.0.0.1:49152".into())]
        );
    }

    #[test]
    fn rejects_nat_map_when_node_and_tunnel_lengths_differ() {
        let nodes = vec![ClusterNode {
            host: "10.0.0.2".into(),
            port: 7000,
        }];
        let tunnels = Vec::new();

        let err = build_nat_map(&nodes, &tunnels).unwrap_err();

        assert!(matches!(
            err,
            crate::error::AppError::RedisClusterFailed(detail)
                if detail == "Cluster NAT map requires matching node and tunnel counts"
        ));
    }

    #[test]
    fn returns_master_targets_for_ssh_fan_out() {
        let nodes = "id1 10.0.0.2:7000@17000 master - 0 0 1 connected 0-5460\nid2 10.0.0.3:7001@17001 master - 0 0 2 connected 5461-10922\n";

        assert_eq!(
            tunnel_targets_for_masters(nodes).unwrap(),
            vec![
                ClusterNode {
                    host: "10.0.0.2".into(),
                    port: 7000
                },
                ClusterNode {
                    host: "10.0.0.3".into(),
                    port: 7001
                },
            ]
        );
    }

    #[tokio::test]
    async fn tunnel_resolver_maps_real_master_endpoint_to_local_tunnel() {
        let nodes = vec![ClusterNode {
            host: "10.0.0.2".into(),
            port: 7000,
        }];
        let tunnels = vec![ClusterNode {
            host: "127.0.0.1".into(),
            port: 49152,
        }];
        let resolver = build_tunnel_resolver(&nodes, &tunnels).unwrap();

        let resolved = resolver
            .resolve("10.0.0.2", 7000)
            .await
            .unwrap()
            .collect::<Vec<_>>();

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].ip().to_string(), "127.0.0.1");
        assert_eq!(resolved[0].port(), 49152);
    }

    #[tokio::test]
    async fn tunnel_resolver_matches_hostnames_case_insensitively() {
        let nodes = vec![ClusterNode {
            host: "RedisA.EXAMPLE.com".into(),
            port: 7000,
        }];
        let tunnels = vec![ClusterNode {
            host: "127.0.0.1".into(),
            port: 49152,
        }];
        let resolver = build_tunnel_resolver(&nodes, &tunnels).unwrap();

        let resolved = resolver
            .resolve("redisa.example.com", 7000)
            .await
            .unwrap()
            .collect::<Vec<_>>();

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].ip().to_string(), "127.0.0.1");
        assert_eq!(resolved[0].port(), 49152);
    }

    #[tokio::test]
    async fn tunnel_resolver_falls_back_to_normal_dns_for_unmapped_endpoint() {
        let resolver = build_tunnel_resolver(&[], &[]).unwrap();

        let resolved = resolver
            .resolve("127.0.0.1", 6379)
            .await
            .unwrap()
            .collect::<Vec<_>>();

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].ip().to_string(), "127.0.0.1");
        assert_eq!(resolved[0].port(), 6379);
    }
}
