use crate::error::{AppError, AppResult};
use crate::models::{ProtoFile, UpdateCheckOptions, UpdateCheckResult, WindowState};
use crate::services;
use crate::services::codec::{
    CodecDecodeOptions, CodecDecodeResult, CodecDetectOptions, CodecDetectResult,
    CodecEncodeOptions, CodecEncodeResult,
};
use crate::services::formatter::{FormatterRunOptions, FormatterRunResult};
use tauri::{Manager, WebviewWindow};
use tauri_plugin_opener::OpenerExt;

#[tauri::command]
pub async fn native_open_external(app: tauri::AppHandle, url: String) -> AppResult<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(AppError::NativeFailed(format!("unsupported url: {url}")));
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| AppError::NativeFailed(e.to_string()))
}

/// 打开随包分发的第三方许可证全文。
///
/// 路径在后端解析，不接受前端传参 —— `native_open_external` 只放行 http/https
/// 就是为了不给渲染进程一个"打开任意本地文件"的口子，这里不能把那个口子从
/// 侧门开回来。
#[tauri::command]
pub async fn native_open_licenses(app: tauri::AppHandle) -> AppResult<()> {
    let path = app
        .path()
        .resolve(LICENSES_FILE, tauri::path::BaseDirectory::Resource)
        .map_err(|e| AppError::NativeFailed(e.to_string()))?;
    if !path.exists() {
        return Err(AppError::NativeFailed(format!(
            "{LICENSES_FILE} is missing from the application resources"
        )));
    }
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|e| AppError::NativeFailed(e.to_string()))
}

/// 与 `tauri.conf.json` 的 `bundle.resources` 对应，改一处要改两处。
const LICENSES_FILE: &str = "THIRD_PARTY_LICENSES.txt";

#[tauri::command]
pub async fn native_minimize_window(window: WebviewWindow) -> AppResult<()> {
    window
        .minimize()
        .map_err(|e| AppError::NativeFailed(e.to_string()))
}

#[tauri::command]
pub async fn native_toggle_maximize(window: WebviewWindow) -> AppResult<()> {
    if window
        .is_maximized()
        .map_err(|e| AppError::NativeFailed(e.to_string()))?
    {
        window
            .unmaximize()
            .map_err(|e| AppError::NativeFailed(e.to_string()))
    } else {
        window
            .maximize()
            .map_err(|e| AppError::NativeFailed(e.to_string()))
    }
}

#[tauri::command]
pub async fn native_get_temp_path(app: tauri::AppHandle) -> AppResult<String> {
    Ok(app
        .path()
        .temp_dir()
        .map_err(|e| AppError::NativeFailed(e.to_string()))?
        .to_string_lossy()
        .to_string())
}

#[tauri::command]
pub async fn native_get_fonts() -> AppResult<Vec<String>> {
    Ok(services::native::list_system_fonts())
}

#[tauri::command]
pub async fn native_set_zoom_factor(value: f64) -> AppResult<f64> {
    if !value.is_finite() || value <= 0.0 {
        return Err(AppError::NativeFailed(format!(
            "invalid zoom factor: {value}"
        )));
    }
    Ok(value)
}

#[tauri::command]
pub async fn native_hide_window(window: WebviewWindow) -> AppResult<()> {
    window
        .hide()
        .map_err(|error| AppError::NativeFailed(error.to_string()))
}

#[tauri::command]
pub async fn native_get_window_state(window: WebviewWindow) -> AppResult<WindowState> {
    Ok(WindowState {
        is_maximized: window
            .is_maximized()
            .map_err(|error| AppError::NativeFailed(error.to_string()))?,
        is_minimized: window
            .is_minimized()
            .map_err(|error| AppError::NativeFailed(error.to_string()))?,
        is_visible: window
            .is_visible()
            .map_err(|error| AppError::NativeFailed(error.to_string()))?,
        scale_factor: window
            .scale_factor()
            .map_err(|error| AppError::NativeFailed(error.to_string()))?,
    })
}

#[tauri::command]
pub async fn native_check_update(
    app: tauri::AppHandle,
    options: UpdateCheckOptions,
) -> AppResult<UpdateCheckResult> {
    let current_version = options
        .current_version
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| app.package_info().version.to_string());
    services::native::check_update(options.endpoint, current_version)
        .await
        .map_err(AppError::NativeFailed)
}

#[tauri::command]
pub async fn native_read_proto_files(paths: Vec<String>) -> AppResult<Vec<ProtoFile>> {
    services::native::read_proto_files(paths).map_err(AppError::NativeFailed)
}

#[tauri::command]
pub async fn native_run_formatter(options: FormatterRunOptions) -> AppResult<FormatterRunResult> {
    services::formatter::run_formatter(options).await
}

#[tauri::command]
pub async fn native_codec_decode(options: CodecDecodeOptions) -> AppResult<CodecDecodeResult> {
    services::codec::decode(options)
}

#[tauri::command]
pub async fn native_codec_encode(options: CodecEncodeOptions) -> AppResult<CodecEncodeResult> {
    services::codec::encode(options)
}

#[tauri::command]
pub async fn native_codec_detect(options: CodecDetectOptions) -> AppResult<CodecDetectResult> {
    services::codec::detect(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{UpdateCheckResult, UpdateCheckStatus, WindowState};

    #[test]
    fn serializes_phase4a_native_models_as_camel_case() {
        let window = WindowState {
            is_maximized: true,
            is_minimized: false,
            is_visible: true,
            scale_factor: 1.25,
        };
        let update = UpdateCheckResult {
            status: UpdateCheckStatus::NotConfigured,
            current_version: "0.1.0".into(),
            latest_version: None,
            release_url: None,
            message: Some("Update check endpoint is not configured".into()),
        };

        assert_eq!(
            serde_json::to_value(window).unwrap(),
            serde_json::json!({
                "isMaximized": true,
                "isMinimized": false,
                "isVisible": true,
                "scaleFactor": 1.25
            })
        );
        assert_eq!(
            serde_json::to_value(update).unwrap(),
            serde_json::json!({
                "status": "notConfigured",
                "currentVersion": "0.1.0",
                "latestVersion": null,
                "releaseUrl": null,
                "message": "Update check endpoint is not configured"
            })
        );
    }

    #[test]
    fn phase4a_native_command_symbols_exist() {
        let _ = native_get_fonts;
        let _ = native_set_zoom_factor;
        let _ = native_hide_window;
        let _ = native_get_window_state;
        let _ = native_check_update;
        let _ = native_read_proto_files;
        let _ = native_run_formatter;
        let _ = native_codec_decode;
        let _ = native_codec_encode;
        let _ = native_codec_detect;
    }
}
