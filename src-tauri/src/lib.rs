pub mod commands;
pub mod error;
pub mod models;
pub mod services;
pub mod state;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .setup(|app| {
            use tauri::{Emitter, Manager};
            let handle = app.handle().clone();
            let _ = app
                .state::<AppState>()
                .capability_sink
                .set(std::sync::Arc::new(move |event| {
                    let _ = handle.emit("redis://capability-change", event);
                }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::native::native_open_external,
            commands::native::native_open_licenses,
            commands::native::native_minimize_window,
            commands::native::native_toggle_maximize,
            commands::native::native_get_temp_path,
            commands::native::native_get_fonts,
            commands::native::native_set_zoom_factor,
            commands::native::native_hide_window,
            commands::native::native_get_window_state,
            commands::native::native_check_update,
            commands::native::native_read_proto_files,
            commands::native::native_run_formatter,
            commands::native::native_codec_decode,
            commands::native::native_codec_encode,
            commands::native::native_codec_detect,
            commands::connection_store::connection_store_list,
            commands::connection_store::connection_store_get,
            commands::connection_store::connection_store_save,
            commands::connection_store::connection_store_delete,
            commands::connection_store::connection_store_reorder,
            commands::connection_store::connection_store_migrate_legacy,
            commands::redis::redis_open_connection,
            commands::redis::redis_open_stored_connection,
            commands::redis::redis_close_connection,
            commands::redis::redis_execute_cli,
            commands::redis::redis_scan_keys,
            commands::redis::redis_get_key_meta,
            commands::redis::redis_get_string_value,
            commands::redis::redis_get_hash_entries,
            commands::redis::redis_get_set_members,
            commands::redis::redis_get_list_range,
            commands::redis::redis_get_zset_range,
            commands::redis::redis_get_stream_entries,
            commands::redis::redis_get_stream_groups,
            commands::redis::redis_get_full_value,
            commands::redis::redis_get_hll_count,
            commands::redis::redis_inspect_hll,
            commands::redis::redis_add_hll_elements,
            commands::redis::redis_set_string_value,
            commands::redis::redis_create_key,
            commands::redis::redis_key_exists,
            commands::redis::redis_start_clone_key,
            commands::redis::redis_get_clone_task,
            commands::redis::redis_set_hash_field,
            commands::redis::redis_delete_hash_fields,
            commands::redis::redis_add_set_members,
            commands::redis::redis_delete_set_members,
            commands::redis::redis_set_list_item,
            commands::redis::redis_push_list_item,
            commands::redis::redis_delete_list_item,
            commands::redis::redis_add_zset_member,
            commands::redis::redis_delete_zset_members,
            commands::redis::redis_set_key_ttl,
            commands::redis::redis_rename_key,
            commands::redis::redis_delete_keys,
            commands::redis::redis_get_database_list,
            commands::redis::redis_get_server_memory,
            commands::redis::redis_cancel_task,
            commands::redis::redis_get_task_status,
            commands::redis::redis_set_task_paused,
            commands::redis::redis_start_load_all_keys,
            commands::redis::redis_start_search_keys,
            commands::redis::redis_start_batch_delete,
            commands::redis::redis_start_batch_ttl,
            commands::redis::redis_start_memory_analysis,
            commands::redis::redis_start_monitor,
            commands::redis::redis_start_subscription,
            commands::redis::redis_start_import_keys,
            commands::redis::redis_start_import_commands,
            commands::redis::redis_start_export_keys,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
