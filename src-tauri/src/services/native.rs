use crate::models::{ProtoFile, UpdateCheckResult, UpdateCheckStatus};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::Duration;

pub fn normalize_font_families(fonts: Vec<String>) -> Vec<String> {
    let mut names = BTreeSet::new();
    for font in fonts {
        let name = font.trim().trim_matches('"').trim().to_string();
        if !name.is_empty() {
            names.insert(name);
        }
    }
    names.into_iter().collect()
}

pub fn list_system_fonts() -> Vec<String> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    normalize_font_families(
        db.faces()
            .flat_map(|face| face.families.iter().map(|(name, _)| name.to_string()))
            .collect(),
    )
}

pub fn update_result_not_configured(current_version: &str) -> UpdateCheckResult {
    UpdateCheckResult {
        status: UpdateCheckStatus::NotConfigured,
        current_version: current_version.to_string(),
        latest_version: None,
        release_url: None,
        message: Some("Update check endpoint is not configured".into()),
    }
}

pub fn update_result_from_json(
    current_version: &str,
    body: &str,
) -> Result<UpdateCheckResult, String> {
    let value: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let latest = value
        .get("latestVersion")
        .or_else(|| value.get("version"))
        .or_else(|| value.get("tag_name"))
        .and_then(Value::as_str)
        .map(normalize_version);
    let release_url = value
        .get("releaseUrl")
        .or_else(|| value.get("release_url"))
        .or_else(|| value.get("html_url"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let Some(latest_version) = latest else {
        return Ok(UpdateCheckResult {
            status: UpdateCheckStatus::Failed,
            current_version: current_version.to_string(),
            latest_version: None,
            release_url,
            message: Some("Update metadata did not include a version".into()),
        });
    };

    let current = parse_update_version(current_version);
    let latest = parse_update_version(&latest_version);
    let (Ok(current), Ok(latest)) = (current, latest) else {
        return Ok(UpdateCheckResult {
            status: UpdateCheckStatus::Failed,
            current_version: current_version.to_string(),
            latest_version: Some(latest_version),
            release_url,
            message: Some("Update metadata included an invalid version".into()),
        });
    };

    let status = if latest > current {
        UpdateCheckStatus::Available
    } else {
        UpdateCheckStatus::NotAvailable
    };

    Ok(UpdateCheckResult {
        status,
        current_version: current_version.to_string(),
        latest_version: Some(latest_version),
        release_url,
        message: None,
    })
}

pub async fn check_update(
    endpoint: Option<String>,
    current_version: String,
) -> Result<UpdateCheckResult, String> {
    let endpoint = endpoint
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let Some(endpoint) = endpoint else {
        return Ok(update_result_not_configured(&current_version));
    };

    let client = reqwest::Client::builder()
        .timeout(update_request_timeout())
        .build()
        .map_err(|error| error.to_string())?;

    let response = client
        .get(endpoint)
        .header(
            "User-Agent",
            concat!("TsubasaRedisManager/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let body = response.text().await.map_err(|error| error.to_string())?;
    update_result_from_json(&current_version, &body)
}

pub fn read_proto_files(paths: Vec<String>) -> Result<Vec<ProtoFile>, String> {
    let mut files = Vec::new();
    for path in paths {
        let path_ref = Path::new(&path);
        let is_proto = path_ref
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.eq_ignore_ascii_case("proto"))
            .unwrap_or(false);
        if !is_proto {
            return Err(format!("Only .proto files can be selected: {path}"));
        }
        let content = fs::read_to_string(path_ref).map_err(|error| error.to_string())?;
        let name = path_ref
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        files.push(ProtoFile {
            path,
            name,
            content,
        });
    }
    Ok(files)
}

fn normalize_version(version: &str) -> String {
    version
        .trim()
        .trim_start_matches('v')
        .trim_start_matches('V')
        .to_string()
}

fn parse_update_version(version: &str) -> Result<semver::Version, semver::Error> {
    semver::Version::parse(&normalize_version(version))
}

fn update_request_timeout() -> Duration {
    Duration::from_secs(10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::UpdateCheckStatus;
    use std::io::Write;
    use tempfile::Builder;

    #[test]
    fn normalizes_font_families_by_trimming_quotes_sorting_and_deduping() {
        let fonts = normalize_font_families(vec![
            "\"JetBrains Mono\"".into(),
            "Arial".into(),
            "JetBrains Mono".into(),
            "".into(),
            "  Segoe UI  ".into(),
        ]);

        assert_eq!(fonts, vec!["Arial", "JetBrains Mono", "Segoe UI"]);
    }

    #[test]
    fn parses_github_release_update_response() {
        let json = r#"{
            "tag_name": "v1.2.3",
            "html_url": "https://example.test/releases/v1.2.3",
            "name": "Release 1.2.3"
        }"#;

        let result = update_result_from_json("0.1.0", json).unwrap();

        assert_eq!(result.status, UpdateCheckStatus::Available);
        assert_eq!(result.current_version, "0.1.0");
        assert_eq!(result.latest_version.as_deref(), Some("1.2.3"));
        assert_eq!(
            result.release_url.as_deref(),
            Some("https://example.test/releases/v1.2.3")
        );
    }

    #[test]
    fn reports_not_available_for_equal_versions() {
        let json = r#"{"latestVersion":"0.1.0","releaseUrl":"https://example.test"}"#;

        let result = update_result_from_json("0.1.0", json).unwrap();

        assert_eq!(result.status, UpdateCheckStatus::NotAvailable);
        assert_eq!(result.latest_version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn reports_failed_for_missing_latest_version() {
        let result = update_result_from_json("0.1.0", "{}").unwrap();

        assert_eq!(result.status, UpdateCheckStatus::Failed);
        assert_eq!(
            result.message.as_deref(),
            Some("Update metadata did not include a version")
        );
    }

    #[test]
    fn reports_failed_for_invalid_latest_version() {
        let json = r#"{"latestVersion":"not-a-version","releaseUrl":"https://example.test"}"#;

        let result = update_result_from_json("0.1.0", json).unwrap();

        assert_eq!(result.status, UpdateCheckStatus::Failed);
        assert_eq!(result.latest_version.as_deref(), Some("not-a-version"));
        assert_eq!(
            result.message.as_deref(),
            Some("Update metadata included an invalid version")
        );
    }

    #[test]
    fn reports_failed_for_invalid_current_version() {
        let json = r#"{"latestVersion":"1.0.0","releaseUrl":"https://example.test"}"#;

        let result = update_result_from_json("not-a-version", json).unwrap();

        assert_eq!(result.status, UpdateCheckStatus::Failed);
        assert_eq!(result.latest_version.as_deref(), Some("1.0.0"));
        assert_eq!(
            result.message.as_deref(),
            Some("Update metadata included an invalid version")
        );
    }

    #[test]
    fn update_client_uses_bounded_timeout() {
        assert_eq!(update_request_timeout(), std::time::Duration::from_secs(10));
    }

    #[test]
    fn reads_only_proto_files_as_utf8_content() {
        let mut file = Builder::new().suffix(".proto").tempfile().unwrap();
        write!(
            file,
            "syntax = \"proto3\"; message User {{ string name = 1; }}"
        )
        .unwrap();

        let files = read_proto_files(vec![file.path().to_string_lossy().to_string()]).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].name,
            file.path().file_name().unwrap().to_string_lossy()
        );
        assert!(files[0].content.contains("message User"));
    }

    #[test]
    fn rejects_non_proto_file_paths() {
        let file = Builder::new().suffix(".txt").tempfile().unwrap();

        let error = read_proto_files(vec![file.path().to_string_lossy().to_string()]).unwrap_err();

        assert!(error.contains("Only .proto files can be selected"));
    }
}
