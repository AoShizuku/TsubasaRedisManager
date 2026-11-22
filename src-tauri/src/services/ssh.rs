use std::sync::Arc;
use std::time::Duration;

use crate::error::{AppError, AppResult};
use crate::models::SshOptions;
use russh::client::{self, Config as SshClientConfig, Handler};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;

pub enum SshAuthMethod {
    Password(String),
    PrivateKey {
        path: String,
        passphrase: Option<String>,
    },
}

#[derive(Clone)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
}

pub struct SshTunnel {
    pub local_host: String,
    pub local_port: u16,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

pub fn choose_auth_method(options: &SshOptions) -> AppResult<SshAuthMethod> {
    if let Some(password) = non_empty_after_trim(options.password.as_deref()) {
        return Ok(SshAuthMethod::Password(password.to_string()));
    }

    if let Some(path) = non_empty_after_trim(options.privatekey.as_deref()) {
        return Ok(SshAuthMethod::PrivateKey {
            path: path.to_string(),
            passphrase: non_empty_after_trim(options.passphrase.as_deref()).map(str::to_string),
        });
    }

    Err(AppError::RedisSshFailed(
        "SSH password or private key is required".into(),
    ))
}

pub async fn open_tunnel(options: &SshOptions, target: SshTarget) -> AppResult<SshTunnel> {
    let duration = Duration::from_secs(options.timeout.unwrap_or(30));
    let auth_method = choose_auth_method(options)?;
    let client_config = Arc::new(SshClientConfig::default());
    let ssh_host = options.host.clone();
    let ssh_port = options.port;
    let username = options.username.clone();

    let mut session = timeout(
        duration,
        client::connect(
            client_config,
            (ssh_host.as_str(), ssh_port),
            AcceptAnyServerKey,
        ),
    )
    .await
    .map_err(|_| {
        AppError::RedisSshFailed(format!(
            "SSH connect to {ssh_host}:{ssh_port} timed out after {} seconds",
            duration.as_secs()
        ))
    })?
    .map_err(|e| {
        AppError::RedisSshFailed(format!("SSH connect to {ssh_host}:{ssh_port} failed: {e}"))
    })?;

    let authenticated = match auth_method {
        SshAuthMethod::Password(password) => {
            timeout(duration, session.authenticate_password(username, password))
                .await
                .map_err(|_| {
                    AppError::RedisSshFailed(format!(
                        "SSH password authentication timed out after {} seconds",
                        duration.as_secs()
                    ))
                })?
                .map_err(|e| {
                    AppError::RedisSshFailed(format!("SSH password authentication failed: {e}"))
                })?
        }
        SshAuthMethod::PrivateKey { path, passphrase } => {
            let key = load_secret_key(&path, passphrase.as_deref()).map_err(|e| {
                AppError::RedisSshFailed(format!(
                    "SSH private key could not be loaded from {path}: {e}"
                ))
            })?;
            let rsa_hash = timeout(duration, session.best_supported_rsa_hash())
                .await
                .map_err(|_| {
                    AppError::RedisSshFailed(format!(
                        "SSH public key negotiation timed out after {} seconds",
                        duration.as_secs()
                    ))
                })?
                .map_err(|e| {
                    AppError::RedisSshFailed(format!("SSH public key negotiation failed: {e}"))
                })?
                .flatten();
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash);
            timeout(duration, session.authenticate_publickey(username, key))
                .await
                .map_err(|_| {
                    AppError::RedisSshFailed(format!(
                        "SSH public key authentication timed out after {} seconds",
                        duration.as_secs()
                    ))
                })?
                .map_err(|e| {
                    AppError::RedisSshFailed(format!("SSH public key authentication failed: {e}"))
                })?
        }
    };

    if !authenticated.success() {
        return Err(AppError::RedisSshFailed(
            "SSH authentication was rejected by the server".into(),
        ));
    }

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| AppError::RedisSshFailed(format!("SSH local listener failed: {e}")))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| AppError::RedisSshFailed(format!("SSH local listener address failed: {e}")))?;
    let local_host = local_addr.ip().to_string();
    let local_port = local_addr.port();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    break;
                }
                accepted = listener.accept() => {
                    let Ok((mut local_socket, _)) = accepted else {
                        break;
                    };
                    let channel = session
                        .channel_open_direct_tcpip(
                            target.host.clone(),
                            target.port as u32,
                            "127.0.0.1",
                            local_port as u32,
                        )
                        .await;
                    let Ok(channel) = channel else {
                        continue;
                    };

                    tokio::spawn(async move {
                        let mut channel_stream = channel.into_stream();
                        let _ = tokio::io::copy_bidirectional(
                            &mut local_socket,
                            &mut channel_stream,
                        )
                        .await;
                    });
                }
            }
        }
    });

    Ok(SshTunnel {
        local_host,
        local_port,
        shutdown: Some(shutdown_tx),
        task,
    })
}

#[derive(Clone, Copy)]
struct AcceptAnyServerKey;

impl Handler for AcceptAnyServerKey {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Phase 2 preserves the existing no-known-hosts behavior; host key
        // verification should be tightened when known_hosts support exists.
        Ok(true)
    }
}

fn non_empty_after_trim(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::{choose_auth_method, SshAuthMethod};
    use crate::models::SshOptions;

    fn ssh_options() -> SshOptions {
        SshOptions {
            host: "ssh.local".into(),
            port: 22,
            username: "redis".into(),
            password: None,
            privatekey: None,
            privatekeybookmark: None,
            passphrase: None,
            timeout: Some(30),
        }
    }

    #[test]
    fn choose_auth_method_prefers_non_empty_password() {
        let options = SshOptions {
            password: Some("secret".into()),
            privatekey: Some("id_rsa".into()),
            passphrase: Some("key-passphrase".into()),
            ..ssh_options()
        };

        let auth = choose_auth_method(&options).expect("password auth should be selected");

        assert!(matches!(auth, SshAuthMethod::Password(password) if password == "secret"));
    }

    #[test]
    fn choose_auth_method_falls_back_to_private_key_with_passphrase() {
        let options = SshOptions {
            password: Some("  ".into()),
            privatekey: Some("id_rsa".into()),
            passphrase: Some("key-passphrase".into()),
            ..ssh_options()
        };

        let auth = choose_auth_method(&options).expect("private key auth should be selected");

        assert!(matches!(
            auth,
            SshAuthMethod::PrivateKey { path, passphrase }
                if path == "id_rsa" && passphrase.as_deref() == Some("key-passphrase")
        ));
    }
}
