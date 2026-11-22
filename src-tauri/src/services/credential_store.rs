use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use keyring::{Entry, Error as KeyringError};

use crate::error::{AppError, AppResult};

const KEYRING_SERVICE_NAME: &str = "com.tsubasa.redismanager.connection";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretField {
    RedisAuth,
    SshPassword,
    SshPassphrase,
    SentinelNodePassword,
}

impl SecretField {
    pub fn as_str(self) -> &'static str {
        match self {
            SecretField::RedisAuth => "redis_auth",
            SecretField::SshPassword => "ssh_password",
            SecretField::SshPassphrase => "ssh_passphrase",
            SecretField::SentinelNodePassword => "sentinel_node_password",
        }
    }

    fn all() -> [SecretField; 4] {
        [
            SecretField::RedisAuth,
            SecretField::SshPassword,
            SecretField::SshPassphrase,
            SecretField::SentinelNodePassword,
        ]
    }
}

pub trait CredentialStore: Send + Sync {
    fn get_secret(&self, connection_key: &str, field: SecretField) -> AppResult<Option<String>>;
    fn set_secret(&self, connection_key: &str, field: SecretField, value: &str) -> AppResult<()>;
    fn delete_secret(&self, connection_key: &str, field: SecretField) -> AppResult<()>;

    fn delete_all_connection_secrets(&self, connection_key: &str) -> AppResult<()> {
        for field in SecretField::all() {
            self.delete_secret(connection_key, field)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemoryCredentialStore {
    secrets: Arc<Mutex<HashMap<(String, SecretField), String>>>,
}

impl CredentialStore for MemoryCredentialStore {
    fn get_secret(&self, connection_key: &str, field: SecretField) -> AppResult<Option<String>> {
        Ok(self
            .secrets
            .lock()
            .map_err(|e| {
                connection_store_error("connection_store.secret_read_failed", e.to_string())
            })?
            .get(&(connection_key.to_string(), field))
            .cloned())
    }

    fn set_secret(&self, connection_key: &str, field: SecretField, value: &str) -> AppResult<()> {
        self.secrets
            .lock()
            .map_err(|e| {
                connection_store_error("connection_store.secret_write_failed", e.to_string())
            })?
            .insert((connection_key.to_string(), field), value.to_string());

        Ok(())
    }

    fn delete_secret(&self, connection_key: &str, field: SecretField) -> AppResult<()> {
        self.secrets
            .lock()
            .map_err(|e| {
                connection_store_error("connection_store.secret_delete_failed", e.to_string())
            })?
            .remove(&(connection_key.to_string(), field));

        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct KeyringCredentialStore;

impl KeyringCredentialStore {
    fn entry(&self, connection_key: &str, field: SecretField) -> AppResult<Entry> {
        Entry::new(KEYRING_SERVICE_NAME, &account_name(connection_key, field)).map_err(|e| {
            connection_store_error("connection_store.secret_unavailable", e.to_string())
        })
    }
}

impl CredentialStore for KeyringCredentialStore {
    fn get_secret(&self, connection_key: &str, field: SecretField) -> AppResult<Option<String>> {
        match self.entry(connection_key, field)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(e) => Err(connection_store_error(
                "connection_store.secret_read_failed",
                e.to_string(),
            )),
        }
    }

    fn set_secret(&self, connection_key: &str, field: SecretField, value: &str) -> AppResult<()> {
        self.entry(connection_key, field)?
            .set_password(value)
            .map_err(|e| {
                connection_store_error("connection_store.secret_write_failed", e.to_string())
            })
    }

    fn delete_secret(&self, connection_key: &str, field: SecretField) -> AppResult<()> {
        match self.entry(connection_key, field)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(e) => Err(connection_store_error(
                "connection_store.secret_delete_failed",
                e.to_string(),
            )),
        }
    }
}

pub fn connection_store_error(code: &str, detail: String) -> AppError {
    let message = match code {
        "connection_store.secret_unavailable" => "Connection secret storage is unavailable",
        "connection_store.secret_write_failed" => "Connection secret could not be saved",
        "connection_store.secret_read_failed" => "Connection secret could not be read",
        "connection_store.secret_delete_failed" => "Connection secret could not be deleted",
        _ => "Connection store operation failed",
    };

    AppError::ConnectionStoreFailed {
        code: code.to_string(),
        message: message.to_string(),
        detail: Some(detail),
    }
}

fn account_name(connection_key: &str, field: SecretField) -> String {
    format!("{}:{}", connection_key, field.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_sets_gets_and_deletes_each_secret_field() {
        let store = MemoryCredentialStore::default();

        store
            .set_secret("conn-1", SecretField::RedisAuth, "redis-secret")
            .unwrap();
        store
            .set_secret("conn-1", SecretField::SshPassword, "ssh-secret")
            .unwrap();
        store
            .set_secret("conn-1", SecretField::SshPassphrase, "phrase-secret")
            .unwrap();
        store
            .set_secret(
                "conn-1",
                SecretField::SentinelNodePassword,
                "sentinel-secret",
            )
            .unwrap();

        assert_eq!(
            store
                .get_secret("conn-1", SecretField::RedisAuth)
                .unwrap()
                .as_deref(),
            Some("redis-secret")
        );
        assert_eq!(
            store
                .get_secret("conn-1", SecretField::SshPassword)
                .unwrap()
                .as_deref(),
            Some("ssh-secret")
        );
        assert_eq!(
            store
                .get_secret("conn-1", SecretField::SshPassphrase)
                .unwrap()
                .as_deref(),
            Some("phrase-secret")
        );
        assert_eq!(
            store
                .get_secret("conn-1", SecretField::SentinelNodePassword)
                .unwrap()
                .as_deref(),
            Some("sentinel-secret")
        );

        store
            .delete_secret("conn-1", SecretField::RedisAuth)
            .unwrap();

        assert_eq!(
            store.get_secret("conn-1", SecretField::RedisAuth).unwrap(),
            None
        );
        assert_eq!(
            store
                .get_secret("conn-1", SecretField::SshPassword)
                .unwrap()
                .as_deref(),
            Some("ssh-secret")
        );
    }

    #[test]
    fn memory_store_deletes_all_connection_secrets() {
        let store = MemoryCredentialStore::default();

        store
            .set_secret("conn-1", SecretField::RedisAuth, "redis-secret")
            .unwrap();
        store
            .set_secret("conn-1", SecretField::SshPassword, "ssh-secret")
            .unwrap();
        store
            .set_secret("conn-2", SecretField::RedisAuth, "other")
            .unwrap();

        store.delete_all_connection_secrets("conn-1").unwrap();

        assert_eq!(
            store.get_secret("conn-1", SecretField::RedisAuth).unwrap(),
            None
        );
        assert_eq!(
            store
                .get_secret("conn-1", SecretField::SshPassword)
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .get_secret("conn-2", SecretField::RedisAuth)
                .unwrap()
                .as_deref(),
            Some("other")
        );
    }
}
