use std::path::PathBuf;

use crate::error::AppResult;
use crate::models::{
    ConnectionEditDraft, ConnectionMigrationResult, ConnectionSaveDraft, ConnectionSummary,
};
use crate::services::connection_store;
use crate::services::credential_store::{connection_store_error, KeyringCredentialStore};
use serde_json::Value;
use tauri::{AppHandle, Manager};

fn connections_path(app: &AppHandle) -> AppResult<PathBuf> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("connections.json"))
        .map_err(|err| connection_store_error("connection_store.load_failed", err.to_string()))
}

#[tauri::command]
pub fn connection_store_list(app: AppHandle) -> AppResult<Vec<ConnectionSummary>> {
    let path = connections_path(&app)?;
    let secrets = KeyringCredentialStore;

    connection_store::list_connections(path, &secrets)
}

#[tauri::command]
pub fn connection_store_get(app: AppHandle, key: String) -> AppResult<ConnectionEditDraft> {
    let path = connections_path(&app)?;
    let secrets = KeyringCredentialStore;

    connection_store::get_connection(path, &secrets, &key)
}

#[tauri::command]
pub fn connection_store_save(
    app: AppHandle,
    draft: ConnectionSaveDraft,
) -> AppResult<ConnectionSummary> {
    let path = connections_path(&app)?;
    let secrets = KeyringCredentialStore;

    connection_store::save_connection(path, &secrets, draft)
}

#[tauri::command]
pub fn connection_store_delete(app: AppHandle, key: String) -> AppResult<()> {
    let path = connections_path(&app)?;
    let secrets = KeyringCredentialStore;

    connection_store::delete_connection(path, &secrets, &key)
}

#[tauri::command]
pub fn connection_store_reorder(
    app: AppHandle,
    keys: Vec<String>,
) -> AppResult<Vec<ConnectionSummary>> {
    let path = connections_path(&app)?;
    let secrets = KeyringCredentialStore;

    connection_store::reorder_connections(path, &secrets, keys)
}

#[tauri::command]
pub fn connection_store_migrate_legacy(
    app: AppHandle,
    payload: Value,
) -> AppResult<ConnectionMigrationResult> {
    let path = connections_path(&app)?;
    let secrets = KeyringCredentialStore;

    connection_store::migrate_legacy_connections(path, &secrets, payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_store_command_symbols_exist() {
        let _ = connection_store_list;
        let _ = connection_store_get;
        let _ = connection_store_save;
        let _ = connection_store_delete;
        let _ = connection_store_reorder;
        let _ = connection_store_migrate_legacy;
    }

    #[test]
    fn connection_store_command_argument_names_match_frontend_payloads() {
        let source = include_str!("connection_store.rs").replace("\r\n", "\n");

        assert!(source.contains("pub fn connection_store_get(app: AppHandle, key: String)"));
        assert!(source.contains("pub fn connection_store_delete(app: AppHandle, key: String)"));
        assert!(source.contains(
            "pub fn connection_store_reorder(\n    app: AppHandle,\n    keys: Vec<String>,"
        ));
    }
}
