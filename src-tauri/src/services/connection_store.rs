use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppResult;
use crate::models::{
    ConnectionConfig, ConnectionEditDraft, ConnectionMigrationResult, ConnectionSaveDraft,
    ConnectionStoreSentinelDraft, ConnectionStoreSshDraft, ConnectionSummary, SentinelOptions,
    SshOptions, SslOptions,
};
use crate::services::credential_store::{connection_store_error, CredentialStore, SecretField};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredConnections {
    #[serde(default)]
    connections: BTreeMap<String, StoredConnectionConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredConnectionConfig {
    name: Option<String>,
    host: String,
    port: u16,
    username: Option<String>,
    db: Option<i64>,
    separator: Option<String>,
    connection_read_only: Option<bool>,
    cluster: Option<bool>,
    color: Option<String>,
    order: Option<i64>,
    ssl_options: Option<SslOptions>,
    ssh_options: Option<StoredSshOptions>,
    sentinel_options: Option<StoredSentinelOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSshOptions {
    host: String,
    port: u16,
    username: String,
    privatekey: Option<String>,
    privatekeybookmark: Option<String>,
    timeout: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSentinelOptions {
    master_name: String,
}

pub fn list_connections(
    path: impl AsRef<Path>,
    secrets: &dyn CredentialStore,
) -> AppResult<Vec<ConnectionSummary>> {
    let store = read_store(path.as_ref())?;
    summaries(&store, secrets)
}

pub fn get_connection(
    path: impl AsRef<Path>,
    secrets: &dyn CredentialStore,
    key: &str,
) -> AppResult<ConnectionEditDraft> {
    let store = read_store(path.as_ref())?;
    let config = store.connections.get(key).ok_or_else(|| {
        connection_store_error(
            "connection_store.not_found",
            format!("connection key: {key}"),
        )
    })?;
    let presence = secret_presence(secrets, key, config)?;

    Ok(ConnectionEditDraft {
        key: Some(key.to_string()),
        name: config.name.clone(),
        host: config.host.clone(),
        port: config.port,
        username: config.username.clone(),
        db: config.db,
        separator: config.separator.clone(),
        connection_read_only: config.connection_read_only,
        cluster: config.cluster,
        color: config.color.clone(),
        order: config.order,
        auth: None,
        ssl_options: config.ssl_options.clone(),
        ssh_options: config.ssh_options.as_ref().map(redacted_ssh_draft),
        sentinel_options: config
            .sentinel_options
            .as_ref()
            .map(redacted_sentinel_draft),
        has_auth: presence.has_auth,
        has_ssh_password: presence.has_ssh_password,
        has_ssh_passphrase: presence.has_ssh_passphrase,
        has_sentinel_node_password: presence.has_sentinel_node_password,
    })
}

pub fn save_connection(
    path: impl AsRef<Path>,
    secrets: &dyn CredentialStore,
    draft: ConnectionSaveDraft,
) -> AppResult<ConnectionSummary> {
    let path = path.as_ref();
    let mut store = read_store(path)?;
    let key = draft
        .key
        .clone()
        .filter(|key| !key.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let previous_config = store.connections.get(&key).cloned();
    let previous_secrets = secret_snapshot(secrets, &key)?;
    let target_secrets = secret_targets(&draft, &previous_secrets);
    validate_draft(&draft, &target_secrets)?;

    let stored = stored_from_draft(&draft);
    store.connections.insert(key.clone(), stored);
    write_store(path, &store)?;
    if let Err(err) = write_secret_targets(secrets, &key, &previous_secrets, &target_secrets) {
        rollback_config(path, &mut store, &key, previous_config);
        return Err(err);
    }

    let config = store
        .connections
        .get(&key)
        .expect("saved connection should exist");
    summary_for(&key, config, secrets)
}

pub fn delete_connection(
    path: impl AsRef<Path>,
    secrets: &dyn CredentialStore,
    key: &str,
) -> AppResult<()> {
    let path = path.as_ref();
    let mut store = read_store(path)?;
    secrets.delete_all_connection_secrets(key)?;
    store.connections.remove(key);
    write_store(path, &store)
}

pub fn reorder_connections(
    path: impl AsRef<Path>,
    secrets: &dyn CredentialStore,
    keys: Vec<String>,
) -> AppResult<Vec<ConnectionSummary>> {
    let path = path.as_ref();
    let mut store = read_store(path)?;
    for (index, key) in keys.iter().enumerate() {
        if let Some(config) = store.connections.get_mut(key) {
            config.order = Some(index as i64);
        }
    }
    write_store(path, &store)?;
    summaries(&store, secrets)
}

pub fn resolve_connection_config(
    path: impl AsRef<Path>,
    secrets: &dyn CredentialStore,
    key: &str,
    db: Option<i64>,
) -> AppResult<ConnectionConfig> {
    let store = read_store(path.as_ref())?;
    let config = store.connections.get(key).ok_or_else(|| {
        connection_store_error(
            "connection_store.not_found",
            format!("connection key: {key}"),
        )
    })?;
    let ssh_options = match &config.ssh_options {
        Some(ssh) => Some(SshOptions {
            host: ssh.host.clone(),
            port: ssh.port,
            username: ssh.username.clone(),
            password: secrets.get_secret(key, SecretField::SshPassword)?,
            privatekey: ssh.privatekey.clone(),
            privatekeybookmark: ssh.privatekeybookmark.clone(),
            passphrase: secrets.get_secret(key, SecretField::SshPassphrase)?,
            timeout: ssh.timeout,
        }),
        None => None,
    };
    let sentinel_options = match &config.sentinel_options {
        Some(sentinel) => Some(SentinelOptions {
            master_name: sentinel.master_name.clone(),
            node_password: secrets.get_secret(key, SecretField::SentinelNodePassword)?,
        }),
        None => None,
    };

    Ok(ConnectionConfig {
        host: config.host.clone(),
        port: config.port,
        auth: secrets.get_secret(key, SecretField::RedisAuth)?,
        username: config.username.clone(),
        db: db.or(config.db),
        name: config.name.clone(),
        separator: config.separator.clone(),
        connection_read_only: config.connection_read_only,
        cluster: config.cluster,
        ssl_options: config.ssl_options.clone(),
        ssh_options,
        sentinel_options,
    })
}

pub fn migrate_legacy_connections(
    path: impl AsRef<Path>,
    secrets: &dyn CredentialStore,
    payload: Value,
) -> AppResult<ConnectionMigrationResult> {
    let path = path.as_ref();
    let store = read_store(path)?;
    let mut known_keys = store.connections.keys().cloned().collect::<Vec<_>>();
    let mut result = ConnectionMigrationResult {
        migrated: Vec::new(),
        migrated_indexes: Vec::new(),
        skipped: Vec::new(),
        failed: Vec::new(),
    };

    for (source_index, fallback_key, value) in legacy_items(payload) {
        let item_key = value
            .get("key")
            .and_then(Value::as_str)
            .filter(|key| !key.trim().is_empty())
            .map(str::to_string)
            .or(fallback_key);
        let failure_key = item_key.clone().unwrap_or_else(|| "<unknown>".into());
        let mut draft = match serde_json::from_value::<ConnectionSaveDraft>(value) {
            Ok(draft) => draft,
            Err(_) => {
                result.failed.push(failure_key);
                continue;
            }
        };
        if draft.key.as_deref().is_none_or(|key| key.trim().is_empty()) {
            draft.key = item_key;
        }
        let key = draft
            .key
            .clone()
            .filter(|key| !key.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        draft.key = Some(key.clone());

        if known_keys.iter().any(|known| known == &key) {
            result.skipped.push(key);
            continue;
        }

        match save_connection(path, secrets, draft) {
            Ok(_) => {
                known_keys.push(key.clone());
                result.migrated.push(key);
                if let Some(index) = source_index {
                    result.migrated_indexes.push(index);
                }
            }
            Err(_) => result.failed.push(key),
        }
    }

    Ok(result)
}

fn read_store(path: &Path) -> AppResult<StoredConnections> {
    if !path.exists() {
        return Ok(StoredConnections::default());
    }

    let raw = fs::read_to_string(path)
        .map_err(|err| connection_store_error("connection_store.load_failed", err.to_string()))?;
    if raw.trim().is_empty() {
        return Ok(StoredConnections::default());
    }

    serde_json::from_str(&raw)
        .map_err(|err| connection_store_error("connection_store.load_failed", err.to_string()))
}

fn write_store(path: &Path, store: &StoredConnections) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            connection_store_error("connection_store.save_failed", err.to_string())
        })?;
    }

    let raw = serde_json::to_string_pretty(store)
        .map_err(|err| connection_store_error("connection_store.save_failed", err.to_string()))?;
    fs::write(path, raw)
        .map_err(|err| connection_store_error("connection_store.save_failed", err.to_string()))
}

fn summaries(
    store: &StoredConnections,
    secrets: &dyn CredentialStore,
) -> AppResult<Vec<ConnectionSummary>> {
    let mut summaries = store
        .connections
        .iter()
        .map(|(key, config)| summary_for(key, config, secrets))
        .collect::<AppResult<Vec<_>>>()?;
    summaries.sort_by(|left, right| {
        let left_order = left.order.unwrap_or(i64::MAX);
        let right_order = right.order.unwrap_or(i64::MAX);
        left_order
            .cmp(&right_order)
            .then_with(|| left.key.cmp(&right.key))
    });
    Ok(summaries)
}

fn summary_for(
    key: &str,
    config: &StoredConnectionConfig,
    secrets: &dyn CredentialStore,
) -> AppResult<ConnectionSummary> {
    let presence = secret_presence(secrets, key, config)?;

    Ok(ConnectionSummary {
        key: key.to_string(),
        name: config
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("{}@{}", config.host, config.port)),
        host: config.host.clone(),
        port: config.port,
        username: config.username.clone(),
        ssh_host: config.ssh_options.as_ref().map(|ssh| ssh.host.clone()),
        ssh_port: config.ssh_options.as_ref().map(|ssh| ssh.port),
        ssh_username: config.ssh_options.as_ref().map(|ssh| ssh.username.clone()),
        sentinel_master_name: config
            .sentinel_options
            .as_ref()
            .map(|sentinel| sentinel.master_name.clone()),
        order: config.order,
        color: config.color.clone(),
        cluster: config.cluster.unwrap_or(false),
        readonly: config.connection_read_only.unwrap_or(false),
        ssh_enabled: config.ssh_options.is_some(),
        tls_enabled: config.ssl_options.is_some(),
        sentinel_enabled: config.sentinel_options.is_some(),
        has_auth: presence.has_auth,
        has_ssh_password: presence.has_ssh_password,
        has_ssh_passphrase: presence.has_ssh_passphrase,
        has_sentinel_node_password: presence.has_sentinel_node_password,
        has_tls_key: config
            .ssl_options
            .as_ref()
            .and_then(|options| non_blank(options.key.as_deref()))
            .is_some(),
        has_tls_cert: config
            .ssl_options
            .as_ref()
            .and_then(|options| non_blank(options.cert.as_deref()))
            .is_some(),
        has_tls_ca: config
            .ssl_options
            .as_ref()
            .and_then(|options| non_blank(options.ca.as_deref()))
            .is_some(),
    })
}

struct SecretPresence {
    has_auth: bool,
    has_ssh_password: bool,
    has_ssh_passphrase: bool,
    has_sentinel_node_password: bool,
}

fn secret_presence(
    secrets: &dyn CredentialStore,
    key: &str,
    config: &StoredConnectionConfig,
) -> AppResult<SecretPresence> {
    Ok(SecretPresence {
        has_auth: secrets.get_secret(key, SecretField::RedisAuth)?.is_some(),
        has_ssh_password: config.ssh_options.is_some()
            && secrets.get_secret(key, SecretField::SshPassword)?.is_some(),
        has_ssh_passphrase: config.ssh_options.is_some()
            && secrets
                .get_secret(key, SecretField::SshPassphrase)?
                .is_some(),
        has_sentinel_node_password: config.sentinel_options.is_some()
            && secrets
                .get_secret(key, SecretField::SentinelNodePassword)?
                .is_some(),
    })
}

fn redacted_ssh_draft(ssh: &StoredSshOptions) -> ConnectionStoreSshDraft {
    ConnectionStoreSshDraft {
        host: ssh.host.clone(),
        port: ssh.port,
        username: ssh.username.clone(),
        password: None,
        clear_password: None,
        privatekey: ssh.privatekey.clone(),
        privatekeybookmark: ssh.privatekeybookmark.clone(),
        passphrase: None,
        clear_passphrase: None,
        timeout: ssh.timeout,
    }
}

fn redacted_sentinel_draft(sentinel: &StoredSentinelOptions) -> ConnectionStoreSentinelDraft {
    ConnectionStoreSentinelDraft {
        master_name: sentinel.master_name.clone(),
        node_password: None,
        clear_node_password: None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SecretSnapshot {
    redis_auth: Option<String>,
    ssh_password: Option<String>,
    ssh_passphrase: Option<String>,
    sentinel_node_password: Option<String>,
}

impl SecretSnapshot {
    fn get(&self, field: SecretField) -> &Option<String> {
        match field {
            SecretField::RedisAuth => &self.redis_auth,
            SecretField::SshPassword => &self.ssh_password,
            SecretField::SshPassphrase => &self.ssh_passphrase,
            SecretField::SentinelNodePassword => &self.sentinel_node_password,
        }
    }
}

fn secret_snapshot(secrets: &dyn CredentialStore, key: &str) -> AppResult<SecretSnapshot> {
    Ok(SecretSnapshot {
        redis_auth: secrets.get_secret(key, SecretField::RedisAuth)?,
        ssh_password: secrets.get_secret(key, SecretField::SshPassword)?,
        ssh_passphrase: secrets.get_secret(key, SecretField::SshPassphrase)?,
        sentinel_node_password: secrets.get_secret(key, SecretField::SentinelNodePassword)?,
    })
}

fn secret_targets(draft: &ConnectionSaveDraft, previous: &SecretSnapshot) -> SecretSnapshot {
    SecretSnapshot {
        redis_auth: target_secret(
            previous.redis_auth.clone(),
            draft.auth.as_deref(),
            draft.clear_auth.unwrap_or(false),
        ),
        ssh_password: draft.ssh_options.as_ref().and_then(|ssh| {
            target_secret(
                previous.ssh_password.clone(),
                ssh.password.as_deref(),
                ssh.clear_password.unwrap_or(false),
            )
        }),
        ssh_passphrase: draft.ssh_options.as_ref().and_then(|ssh| {
            target_secret(
                previous.ssh_passphrase.clone(),
                ssh.passphrase.as_deref(),
                ssh.clear_passphrase.unwrap_or(false),
            )
        }),
        sentinel_node_password: draft.sentinel_options.as_ref().and_then(|sentinel| {
            target_secret(
                previous.sentinel_node_password.clone(),
                sentinel.node_password.as_deref(),
                sentinel.clear_node_password.unwrap_or(false),
            )
        }),
    }
}

fn target_secret(previous: Option<String>, value: Option<&str>, clear: bool) -> Option<String> {
    if clear {
        return None;
    }
    non_blank(value).map(str::to_string).or(previous)
}

fn write_secret_targets(
    secrets: &dyn CredentialStore,
    key: &str,
    previous: &SecretSnapshot,
    target: &SecretSnapshot,
) -> AppResult<()> {
    for field in secret_fields() {
        if previous.get(field) == target.get(field) {
            continue;
        }

        if let Err(err) = write_secret_state(secrets, key, field, target.get(field).as_deref()) {
            restore_secret_snapshot(secrets, key, previous);
            return Err(err);
        }
    }

    Ok(())
}

fn write_secret_state(
    secrets: &dyn CredentialStore,
    key: &str,
    field: SecretField,
    value: Option<&str>,
) -> AppResult<()> {
    match value {
        Some(value) => secrets.set_secret(key, field, value),
        None => secrets.delete_secret(key, field),
    }
}

fn restore_secret_snapshot(secrets: &dyn CredentialStore, key: &str, snapshot: &SecretSnapshot) {
    for field in secret_fields() {
        let _ = write_secret_state(secrets, key, field, snapshot.get(field).as_deref());
    }
}

fn secret_fields() -> [SecretField; 4] {
    [
        SecretField::RedisAuth,
        SecretField::SshPassword,
        SecretField::SshPassphrase,
        SecretField::SentinelNodePassword,
    ]
}

fn rollback_config(
    path: &Path,
    store: &mut StoredConnections,
    key: &str,
    previous_config: Option<StoredConnectionConfig>,
) {
    match previous_config {
        Some(previous_config) => {
            store.connections.insert(key.to_string(), previous_config);
        }
        None => {
            store.connections.remove(key);
        }
    }
    let _ = write_store(path, store);
}

fn stored_from_draft(draft: &ConnectionSaveDraft) -> StoredConnectionConfig {
    StoredConnectionConfig {
        name: draft.name.clone(),
        host: draft.host.clone(),
        port: draft.port,
        username: draft.username.clone(),
        db: draft.db,
        separator: draft.separator.clone(),
        connection_read_only: draft.connection_read_only,
        cluster: draft.cluster,
        color: draft.color.clone(),
        order: draft.order,
        ssl_options: draft.ssl_options.clone(),
        ssh_options: draft.ssh_options.clone().map(|ssh| StoredSshOptions {
            host: ssh.host,
            port: ssh.port,
            username: ssh.username,
            privatekey: ssh.privatekey,
            privatekeybookmark: ssh.privatekeybookmark,
            timeout: ssh.timeout,
        }),
        sentinel_options: draft
            .sentinel_options
            .clone()
            .map(|sentinel| StoredSentinelOptions {
                master_name: sentinel.master_name,
            }),
    }
}

fn validate_draft(draft: &ConnectionSaveDraft, target_secrets: &SecretSnapshot) -> AppResult<()> {
    if draft.host.trim().is_empty() {
        return Err(invalid_config("Connection host is required"));
    }
    if draft.cluster.unwrap_or(false) && draft.sentinel_options.is_some() {
        return Err(invalid_config(
            "Redis Cluster connections cannot use Sentinel options",
        ));
    }
    if let Some(ssh) = &draft.ssh_options {
        if ssh.host.trim().is_empty() || ssh.username.trim().is_empty() {
            return Err(invalid_config("SSH host and username are required"));
        }
        if non_blank(target_secrets.ssh_password.as_deref()).is_none()
            && non_blank(ssh.privatekey.as_deref()).is_none()
        {
            return Err(invalid_config("SSH password or private key is required"));
        }
    }
    if let Some(sentinel) = &draft.sentinel_options {
        if sentinel.master_name.trim().is_empty() {
            return Err(invalid_config("Sentinel master name is required"));
        }
    }

    Ok(())
}

fn invalid_config(detail: &str) -> crate::error::AppError {
    connection_store_error("connection_store.invalid_config", detail.to_string())
}

fn legacy_items(payload: Value) -> Vec<(Option<usize>, Option<String>, Value)> {
    match payload {
        Value::Array(items) => items
            .into_iter()
            .enumerate()
            .map(|(index, item)| (Some(index), None, item))
            .collect(),
        Value::Object(map) => map
            .into_iter()
            .map(|(key, value)| (None, Some(key), value))
            .collect(),
        other => vec![(None, None, other)],
    }
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use crate::error::AppResult;
    use crate::models::{
        ConnectionSaveDraft, ConnectionStoreSentinelDraft, ConnectionStoreSshDraft, SslOptions,
    };
    use crate::services::credential_store::{CredentialStore, MemoryCredentialStore, SecretField};
    use tempfile::{NamedTempFile, TempDir};

    #[derive(Clone, Default)]
    struct FailingCredentialStore {
        secrets: Arc<Mutex<HashMap<(String, SecretField), String>>>,
        fail_set_field: Option<SecretField>,
        fail_delete_field: Option<SecretField>,
    }

    impl FailingCredentialStore {
        fn fail_on_set(field: SecretField) -> Self {
            Self {
                secrets: Arc::default(),
                fail_set_field: Some(field),
                fail_delete_field: None,
            }
        }

        fn fail_on_delete(field: SecretField) -> Self {
            Self {
                secrets: Arc::default(),
                fail_set_field: None,
                fail_delete_field: Some(field),
            }
        }
    }

    impl CredentialStore for FailingCredentialStore {
        fn get_secret(
            &self,
            connection_key: &str,
            field: SecretField,
        ) -> AppResult<Option<String>> {
            Ok(self
                .secrets
                .lock()
                .unwrap()
                .get(&(connection_key.to_string(), field))
                .cloned())
        }

        fn set_secret(
            &self,
            connection_key: &str,
            field: SecretField,
            value: &str,
        ) -> AppResult<()> {
            if self.fail_set_field == Some(field) {
                return Err(connection_store_error(
                    "connection_store.secret_write_failed",
                    format!("forced failure for {}", field.as_str()),
                ));
            }

            self.secrets
                .lock()
                .unwrap()
                .insert((connection_key.to_string(), field), value.to_string());
            Ok(())
        }

        fn delete_secret(&self, connection_key: &str, field: SecretField) -> AppResult<()> {
            if self.fail_delete_field == Some(field) {
                return Err(connection_store_error(
                    "connection_store.secret_delete_failed",
                    format!("forced failure for {}", field.as_str()),
                ));
            }

            self.secrets
                .lock()
                .unwrap()
                .remove(&(connection_key.to_string(), field));
            Ok(())
        }
    }

    fn temp_store_path() -> NamedTempFile {
        NamedTempFile::new().expect("temp store file should be created")
    }

    fn draft(key: Option<&str>) -> ConnectionSaveDraft {
        ConnectionSaveDraft {
            key: key.map(str::to_string),
            name: Some("Production Redis".into()),
            host: "redis.local".into(),
            port: 6380,
            username: Some("default".into()),
            db: Some(2),
            separator: Some(":".into()),
            connection_read_only: Some(true),
            cluster: Some(false),
            color: Some("#2f80ed".into()),
            order: Some(7),
            auth: Some("redis-secret".into()),
            clear_auth: None,
            ssl_options: Some(SslOptions {
                key: Some("tls-key".into()),
                cert: Some("tls-cert".into()),
                ca: Some("tls-ca".into()),
                servername: Some("redis-sni.local".into()),
                insecure: Some(true),
            }),
            ssh_options: Some(ConnectionStoreSshDraft {
                host: "bastion.local".into(),
                port: 22,
                username: "ops".into(),
                password: Some("ssh-password".into()),
                clear_password: None,
                privatekey: Some("fixtures/id_ed25519".into()),
                privatekeybookmark: Some("bookmark".into()),
                passphrase: Some("ssh-passphrase".into()),
                clear_passphrase: None,
                timeout: Some(30),
            }),
            sentinel_options: Some(ConnectionStoreSentinelDraft {
                master_name: "mymaster".into(),
                node_password: Some("sentinel-secret".into()),
                clear_node_password: None,
            }),
        }
    }

    #[test]
    fn save_lists_redacted_summary_and_edit_draft_without_secret_plaintext() {
        let path = temp_store_path();
        let secrets = MemoryCredentialStore::default();

        let summary = save_connection(path.path(), &secrets, draft(Some("conn-1"))).unwrap();
        let summaries = list_connections(path.path(), &secrets).unwrap();
        let edit = get_connection(path.path(), &secrets, "conn-1").unwrap();
        let raw_file = std::fs::read_to_string(path.path()).unwrap();

        assert_eq!(summary.key, "conn-1");
        assert_eq!(summaries, vec![summary.clone()]);
        assert_eq!(summary.name, "Production Redis");
        assert_eq!(summary.ssh_host.as_deref(), Some("bastion.local"));
        assert_eq!(summary.ssh_port, Some(22));
        assert_eq!(summary.ssh_username.as_deref(), Some("ops"));
        assert_eq!(summary.sentinel_master_name.as_deref(), Some("mymaster"));
        assert!(!summary.cluster);
        assert!(summary.readonly);
        assert!(summary.ssh_enabled);
        assert!(summary.tls_enabled);
        assert!(summary.sentinel_enabled);
        assert!(summary.has_auth);
        assert!(summary.has_ssh_password);
        assert!(summary.has_ssh_passphrase);
        assert!(summary.has_sentinel_node_password);
        assert!(summary.has_tls_key);
        assert!(summary.has_tls_cert);
        assert!(summary.has_tls_ca);

        assert_eq!(edit.key.as_deref(), Some("conn-1"));
        assert_eq!(edit.auth, None);
        assert_eq!(edit.ssh_options.as_ref().unwrap().password, None);
        assert_eq!(edit.ssh_options.as_ref().unwrap().passphrase, None);
        assert_eq!(edit.sentinel_options.as_ref().unwrap().node_password, None);
        assert!(edit.has_auth);
        assert!(edit.has_ssh_password);
        assert!(edit.has_ssh_passphrase);
        assert!(edit.has_sentinel_node_password);

        assert!(!raw_file.contains("redis-secret"));
        assert!(!raw_file.contains("ssh-password"));
        assert!(!raw_file.contains("ssh-passphrase"));
        assert!(!raw_file.contains("sentinel-secret"));
    }

    #[test]
    fn blank_secret_update_preserves_existing_secret_and_clear_flag_deletes_it() {
        let path = temp_store_path();
        let secrets = MemoryCredentialStore::default();
        save_connection(path.path(), &secrets, draft(Some("conn-1"))).unwrap();

        let mut update = draft(Some("conn-1"));
        update.auth = Some("  ".into());
        update.ssh_options.as_mut().unwrap().password = Some("".into());
        update.ssh_options.as_mut().unwrap().passphrase = None;
        update.sentinel_options.as_mut().unwrap().node_password = Some("   ".into());
        let summary = save_connection(path.path(), &secrets, update).unwrap();

        assert!(summary.has_auth);
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::RedisAuth)
                .unwrap()
                .as_deref(),
            Some("redis-secret")
        );
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::SshPassword)
                .unwrap()
                .as_deref(),
            Some("ssh-password")
        );
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::SshPassphrase)
                .unwrap()
                .as_deref(),
            Some("ssh-passphrase")
        );
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::SentinelNodePassword)
                .unwrap()
                .as_deref(),
            Some("sentinel-secret")
        );

        let mut clear = draft(Some("conn-1"));
        clear.auth = None;
        clear.clear_auth = Some(true);
        clear.ssh_options.as_mut().unwrap().password = None;
        clear.ssh_options.as_mut().unwrap().clear_password = Some(true);
        clear.ssh_options.as_mut().unwrap().passphrase = None;
        clear.ssh_options.as_mut().unwrap().clear_passphrase = Some(true);
        clear.sentinel_options.as_mut().unwrap().node_password = None;
        clear.sentinel_options.as_mut().unwrap().clear_node_password = Some(true);
        let summary = save_connection(path.path(), &secrets, clear).unwrap();

        assert!(!summary.has_auth);
        assert!(!summary.has_ssh_password);
        assert!(!summary.has_ssh_passphrase);
        assert!(!summary.has_sentinel_node_password);
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::RedisAuth)
                .unwrap(),
            None
        );
    }

    #[test]
    fn save_secret_write_failure_rolls_back_config_and_partial_secrets() {
        let path = temp_store_path();
        let secrets = FailingCredentialStore::fail_on_set(SecretField::SshPassword);

        let err = save_connection(path.path(), &secrets, draft(Some("conn-1"))).unwrap_err();

        assert_eq!(err.payload().code, "connection_store.secret_write_failed");
        assert!(list_connections(path.path(), &secrets).unwrap().is_empty());
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::RedisAuth)
                .unwrap(),
            None
        );
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::SshPassword)
                .unwrap(),
            None
        );
    }

    #[test]
    fn edit_secret_write_failure_restores_previous_config_and_secret_snapshot() {
        let path = temp_store_path();
        let mut secrets = FailingCredentialStore::default();
        save_connection(path.path(), &secrets, draft(Some("conn-1"))).unwrap();
        secrets.fail_set_field = Some(SecretField::SshPassword);

        let mut update = draft(Some("conn-1"));
        update.name = Some("Changed".into());
        update.auth = Some("new-redis-secret".into());
        update.ssh_options.as_mut().unwrap().password = Some("new-ssh-secret".into());
        let err = save_connection(path.path(), &secrets, update).unwrap_err();

        assert_eq!(err.payload().code, "connection_store.secret_write_failed");
        let edit = get_connection(path.path(), &secrets, "conn-1").unwrap();
        assert_eq!(edit.name.as_deref(), Some("Production Redis"));
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::RedisAuth)
                .unwrap()
                .as_deref(),
            Some("redis-secret")
        );
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::SshPassword)
                .unwrap()
                .as_deref(),
            Some("ssh-password")
        );
    }

    #[test]
    fn migration_secret_write_failure_rolls_back_config_and_partial_secrets() {
        let path = temp_store_path();
        let secrets = FailingCredentialStore::fail_on_set(SecretField::SshPassword);
        let payload = serde_json::json!([
            {
                "key": "legacy-1",
                "host": "legacy.local",
                "port": 6379,
                "auth": "legacy-secret",
                "sshOptions": {
                    "host": "bastion.local",
                    "port": 22,
                    "username": "ops",
                    "password": "ssh-secret"
                }
            }
        ]);

        let result = migrate_legacy_connections(path.path(), &secrets, payload).unwrap();

        assert!(result.migrated.is_empty());
        assert_eq!(result.failed, vec!["legacy-1"]);
        assert!(list_connections(path.path(), &secrets).unwrap().is_empty());
        assert_eq!(
            secrets
                .get_secret("legacy-1", SecretField::RedisAuth)
                .unwrap(),
            None
        );
    }

    #[test]
    fn save_write_failure_uses_save_failed_and_does_not_write_orphan_secrets() {
        let parent_file = temp_store_path();
        let path = parent_file.path().join("connections.json");
        let secrets = MemoryCredentialStore::default();

        let err = save_connection(&path, &secrets, draft(Some("conn-1"))).unwrap_err();

        assert_eq!(err.payload().code, "connection_store.save_failed");
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::RedisAuth)
                .unwrap(),
            None
        );
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::SshPassword)
                .unwrap(),
            None
        );
    }

    #[test]
    fn migration_write_failure_records_failure_without_orphan_secrets() {
        let parent_file = temp_store_path();
        let path = parent_file.path().join("connections.json");
        let secrets = MemoryCredentialStore::default();
        let payload = serde_json::json!([
            {
                "key": "legacy-1",
                "host": "legacy.local",
                "port": 6379,
                "auth": "legacy-secret"
            }
        ]);

        let result = migrate_legacy_connections(&path, &secrets, payload).unwrap();

        assert!(result.migrated.is_empty());
        assert_eq!(result.failed, vec!["legacy-1"]);
        assert_eq!(
            secrets
                .get_secret("legacy-1", SecretField::RedisAuth)
                .unwrap(),
            None
        );
    }

    #[test]
    fn load_failure_uses_load_failed_code() {
        let path = temp_store_path();
        std::fs::write(path.path(), "{ not json").unwrap();
        let secrets = MemoryCredentialStore::default();

        let err = list_connections(path.path(), &secrets).unwrap_err();

        assert_eq!(err.payload().code, "connection_store.load_failed");
    }

    #[test]
    fn invalid_save_config_uses_invalid_config_code_and_does_not_write_secret() {
        let path = temp_store_path();
        let secrets = MemoryCredentialStore::default();
        let mut invalid = draft(Some("conn-1"));
        invalid.host = "  ".into();

        let err = save_connection(path.path(), &secrets, invalid).unwrap_err();

        assert_eq!(err.payload().code, "connection_store.invalid_config");
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::RedisAuth)
                .unwrap(),
            None
        );
        assert!(list_connections(path.path(), &secrets).unwrap().is_empty());
    }

    #[test]
    fn invalid_ssh_save_config_requires_auth_method() {
        let path = temp_store_path();
        let secrets = MemoryCredentialStore::default();
        let mut invalid = draft(Some("conn-1"));
        let ssh = invalid.ssh_options.as_mut().unwrap();
        ssh.password = None;
        ssh.privatekey = Some(" ".into());

        let err = save_connection(path.path(), &secrets, invalid).unwrap_err();

        assert_eq!(err.payload().code, "connection_store.invalid_config");
    }

    #[test]
    fn delete_connection_removes_config_and_related_secrets() {
        let path = temp_store_path();
        let secrets = MemoryCredentialStore::default();
        save_connection(path.path(), &secrets, draft(Some("conn-1"))).unwrap();
        save_connection(path.path(), &secrets, draft(Some("conn-2"))).unwrap();

        delete_connection(path.path(), &secrets, "conn-1").unwrap();

        let summaries = list_connections(path.path(), &secrets).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].key, "conn-2");
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::RedisAuth)
                .unwrap(),
            None
        );
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::SshPassword)
                .unwrap(),
            None
        );
        assert_eq!(
            secrets
                .get_secret("conn-2", SecretField::RedisAuth)
                .unwrap()
                .as_deref(),
            Some("redis-secret")
        );
    }

    #[test]
    fn delete_secret_failure_keeps_config_for_retry() {
        let path = temp_store_path();
        let secrets = FailingCredentialStore::fail_on_delete(SecretField::RedisAuth);
        save_connection(path.path(), &secrets, draft(Some("conn-1"))).unwrap();

        let err = delete_connection(path.path(), &secrets, "conn-1").unwrap_err();

        assert_eq!(err.payload().code, "connection_store.secret_delete_failed");
        assert_eq!(list_connections(path.path(), &secrets).unwrap().len(), 1);
        assert_eq!(
            secrets
                .get_secret("conn-1", SecretField::RedisAuth)
                .unwrap()
                .as_deref(),
            Some("redis-secret")
        );
    }

    #[test]
    fn legacy_migration_handles_object_payload_and_is_idempotent() {
        let path = temp_store_path();
        let secrets = MemoryCredentialStore::default();
        let payload = serde_json::json!({
            "legacy-1": {
                "host": "legacy.local",
                "port": 6379,
                "name": "Legacy Redis",
                "auth": "legacy-secret",
                "sshOptions": {
                    "host": "bastion.local",
                    "port": 2222,
                    "username": "deploy",
                    "password": "ssh-secret",
                    "passphrase": "phrase-secret"
                },
                "sentinelOptions": {
                    "masterName": "mymaster",
                    "nodePassword": "node-secret"
                }
            }
        });

        let first = migrate_legacy_connections(path.path(), &secrets, payload.clone()).unwrap();
        let second = migrate_legacy_connections(path.path(), &secrets, payload).unwrap();

        assert_eq!(first.migrated, vec!["legacy-1"]);
        assert!(first.skipped.is_empty());
        assert!(first.failed.is_empty());
        assert!(second.migrated.is_empty());
        assert_eq!(second.skipped, vec!["legacy-1"]);
        assert!(second.failed.is_empty());
        assert_eq!(
            secrets
                .get_secret("legacy-1", SecretField::RedisAuth)
                .unwrap()
                .as_deref(),
            Some("legacy-secret")
        );
        assert_eq!(
            secrets
                .get_secret("legacy-1", SecretField::SshPassword)
                .unwrap()
                .as_deref(),
            Some("ssh-secret")
        );
        assert_eq!(
            secrets
                .get_secret("legacy-1", SecretField::SshPassphrase)
                .unwrap()
                .as_deref(),
            Some("phrase-secret")
        );
        assert_eq!(
            secrets
                .get_secret("legacy-1", SecretField::SentinelNodePassword)
                .unwrap()
                .as_deref(),
            Some("node-secret")
        );
    }

    #[test]
    fn keyless_legacy_array_migration_reports_persisted_generated_key() {
        let path = temp_store_path();
        let secrets = MemoryCredentialStore::default();
        let payload = serde_json::json!([
            {
                "host": "legacy.local",
                "port": 6379,
                "auth": "legacy-secret"
            }
        ]);

        let result = migrate_legacy_connections(path.path(), &secrets, payload).unwrap();
        let returned_key = result.migrated[0].clone();
        let summaries = list_connections(path.path(), &secrets).unwrap();

        assert_eq!(result.migrated.len(), 1);
        assert_eq!(result.migrated_indexes, vec![0]);
        assert!(result.skipped.is_empty());
        assert!(result.failed.is_empty());
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].key, returned_key);
        assert_eq!(
            secrets
                .get_secret(&returned_key, SecretField::RedisAuth)
                .unwrap()
                .as_deref(),
            Some("legacy-secret")
        );
    }

    #[test]
    fn resolve_connection_config_injects_secrets_and_db() {
        let path = temp_store_path();
        let secrets = MemoryCredentialStore::default();
        save_connection(path.path(), &secrets, draft(Some("conn-1"))).unwrap();

        let config = resolve_connection_config(path.path(), &secrets, "conn-1", Some(12)).unwrap();

        assert_eq!(config.host, "redis.local");
        assert_eq!(config.port, 6380);
        assert_eq!(config.db, Some(12));
        assert_eq!(config.auth.as_deref(), Some("redis-secret"));
        assert_eq!(
            config.ssh_options.as_ref().unwrap().password.as_deref(),
            Some("ssh-password")
        );
        assert_eq!(
            config.ssh_options.as_ref().unwrap().passphrase.as_deref(),
            Some("ssh-passphrase")
        );
        assert_eq!(
            config
                .sentinel_options
                .as_ref()
                .unwrap()
                .node_password
                .as_deref(),
            Some("sentinel-secret")
        );
    }

    #[test]
    fn reorder_connections_updates_order() {
        let path = temp_store_path();
        let secrets = MemoryCredentialStore::default();
        let mut first = draft(Some("a"));
        first.order = Some(10);
        let mut second = draft(Some("b"));
        second.order = Some(20);
        save_connection(path.path(), &secrets, first).unwrap();
        save_connection(path.path(), &secrets, second).unwrap();

        let summaries =
            reorder_connections(path.path(), &secrets, vec!["b".into(), "a".into()]).unwrap();

        assert_eq!(
            summaries
                .iter()
                .map(|summary| summary.key.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        assert_eq!(summaries[0].order, Some(0));
        assert_eq!(summaries[1].order, Some(1));
    }

    #[test]
    fn malformed_legacy_item_records_failure_without_blocking_valid_items() {
        let path = temp_store_path();
        let secrets = MemoryCredentialStore::default();
        let payload = serde_json::json!([
            {
                "key": "valid",
                "host": "redis.local",
                "port": 6379,
                "auth": "valid-secret"
            },
            {
                "key": "bad",
                "host": "missing-port"
            }
        ]);

        let result = migrate_legacy_connections(path.path(), &secrets, payload).unwrap();

        assert_eq!(result.migrated, vec!["valid"]);
        assert!(result.skipped.is_empty());
        assert_eq!(result.failed, vec!["bad"]);
        assert_eq!(list_connections(path.path(), &secrets).unwrap().len(), 1);
        assert_eq!(
            secrets
                .get_secret("valid", SecretField::RedisAuth)
                .unwrap()
                .as_deref(),
            Some("valid-secret")
        );
    }

    #[test]
    fn semantically_invalid_legacy_item_records_failure_without_secret_plaintext() {
        let path = temp_store_path();
        let secrets = MemoryCredentialStore::default();
        let payload = serde_json::json!([
            {
                "key": "valid",
                "host": "redis.local",
                "port": 6379
            },
            {
                "key": "invalid",
                "host": "redis.local",
                "port": 6379,
                "cluster": true,
                "auth": "invalid-secret",
                "sentinelOptions": {
                    "masterName": "mymaster",
                    "nodePassword": "node-secret"
                }
            }
        ]);

        let result = migrate_legacy_connections(path.path(), &secrets, payload).unwrap();

        assert_eq!(result.migrated, vec!["valid"]);
        assert_eq!(result.failed, vec!["invalid"]);
        assert!(!result.failed.iter().any(|entry| entry.contains("secret")));
        assert_eq!(
            secrets
                .get_secret("invalid", SecretField::RedisAuth)
                .unwrap(),
            None
        );
        assert_eq!(list_connections(path.path(), &secrets).unwrap().len(), 1);
    }

    #[test]
    fn directory_path_load_failure_uses_load_failed_code() {
        let dir = TempDir::new().unwrap();
        let secrets = MemoryCredentialStore::default();

        let err = list_connections(dir.path(), &secrets).unwrap_err();

        assert_eq!(err.payload().code, "connection_store.load_failed");
    }
}
