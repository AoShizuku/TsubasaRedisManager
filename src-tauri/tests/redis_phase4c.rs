use base64::Engine;
use serial_test::serial;
use tsubasa_redis_manager_lib::models::{ConnectionConfig, RedisText};
use tsubasa_redis_manager_lib::services::codec::{
    decode, encode, CodecDecodeOptions, CodecEncodeOptions, CodecName,
};
use tsubasa_redis_manager_lib::services::connection::{close_connection, open_connection};
use tsubasa_redis_manager_lib::services::values::{
    delete_keys, get_string_value, set_string_value,
};
use tsubasa_redis_manager_lib::state::AppState;

const LOCAL_VDS_ENV_HELP: &str = "Copy scripts/ci/local-vds.env.example.ps1 to scripts/ci/local-vds.env.ps1 for ignored VDS E2E tests.";
const FIXTURE: &str = "{\"phase\":\"4c\",\"value\":\"redis binary round trip\"}";

fn optional_env_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn redis_host() -> String {
    optional_env_string("PHASE4C_REDIS_HOST")
        .or_else(|| optional_env_string("PHASE2_HOST"))
        .unwrap_or_else(|| {
            panic!(
                "PHASE4C_REDIS_HOST or PHASE2_HOST must be set to run ignored VDS E2E tests. {LOCAL_VDS_ENV_HELP}"
            );
        })
}

fn redis_port() -> u16 {
    std::env::var("PHASE4C_REDIS_PORT")
        .or_else(|_| std::env::var("PHASE2_STANDALONE_PORT"))
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(16379)
}

fn config() -> ConnectionConfig {
    ConnectionConfig {
        host: redis_host(),
        port: redis_port(),
        auth: None,
        username: None,
        db: Some(13),
        name: Some("phase4c".into()),
        separator: Some(":".into()),
        connection_read_only: Some(false),
        cluster: None,
        ssl_options: None,
        ssh_options: None,
        sentinel_options: None,
    }
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn new(keys: &[&'static str]) -> Self {
        let saved = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

fn catch_expected_panic<F, T>(operation: F) -> String
where
    F: FnOnce() -> T + std::panic::UnwindSafe,
{
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(operation);
    std::panic::set_hook(previous_hook);

    let payload = match result {
        Ok(_) => panic!("operation should panic"),
        Err(payload) => payload,
    };

    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        message.to_string()
    } else {
        "non-string panic".into()
    }
}

#[test]
#[serial]
fn phase4c_host_requires_explicit_env_when_missing() {
    let _guard = EnvGuard::new(&["PHASE4C_REDIS_HOST", "PHASE2_HOST"]);
    std::env::remove_var("PHASE4C_REDIS_HOST");
    std::env::remove_var("PHASE2_HOST");

    let message = catch_expected_panic(config);

    assert!(message.contains("PHASE4C_REDIS_HOST or PHASE2_HOST"));
    assert!(message.contains("scripts/ci/local-vds.env.example.ps1"));
}

#[test]
#[serial]
fn phase4c_host_requires_explicit_env_when_blank() {
    let _guard = EnvGuard::new(&["PHASE4C_REDIS_HOST", "PHASE2_HOST"]);
    std::env::set_var("PHASE4C_REDIS_HOST", " ");
    std::env::set_var("PHASE2_HOST", "");

    let message = catch_expected_panic(config);

    assert!(message.contains("PHASE4C_REDIS_HOST or PHASE2_HOST"));
    assert!(message.contains("scripts/ci/local-vds.env.ps1"));
}

fn codec_cases() -> Vec<(CodecName, &'static str)> {
    vec![
        (CodecName::Gzip, "gzip"),
        (CodecName::Deflate, "deflate"),
        (CodecName::DeflateRaw, "deflateRaw"),
        (CodecName::Brotli, "brotli"),
    ]
}

/// 压缩过的字节必然不是合法 UTF-8，所以 `RedisText` 走的是 base64 那一支。
/// 这里同时接受两支 —— 断言留给调用方，解析不该自己先失败。
fn text_to_base64(codec_name: &str, value: &RedisText) -> Result<String, String> {
    if let Some(base64) = value.base64.as_deref() {
        return Ok(base64.to_string());
    }
    if let Some(text) = value.text.as_deref() {
        return Ok(base64::engine::general_purpose::STANDARD.encode(text.as_bytes()));
    }
    Err(format!("{codec_name} stored value came back empty"))
}

async fn cleanup_keys(
    state: &AppState,
    connection_id: &str,
    keys: &[String],
) -> Result<(), String> {
    delete_keys(state, connection_id, keys)
        .await
        .map(|_| ())
        .map_err(|err| format!("{err}"))
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn compression_codecs_round_trip_through_vds_redis_binary_value() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let keys = codec_cases()
        .into_iter()
        .map(|(_, name)| format!("phase4c:codec:{token}:{name}"))
        .collect::<Vec<_>>();

    let operation_result = async {
        for ((codec, name), key) in codec_cases().into_iter().zip(keys.iter()) {
            let encoded = encode(CodecEncodeOptions {
                codec,
                input_base64: base64::engine::general_purpose::STANDARD.encode(FIXTURE),
            })
            .map_err(|err| format!("{name} should encode fixture: {err}"))?;

            // 写入走语义化命令：它的入参本来就是 base64 字节，
            // 这正是"把一个压缩值原样写回去"必须走字节而不是文本的原因。
            set_string_value(&state, &opened.connection_id, key, &encoded.output_base64)
                .await
                .map_err(|err| format!("storing {name} bytes should succeed: {err}"))?;

            let stored = get_string_value(&state, &opened.connection_id, key)
                .await
                .map_err(|err| format!("reading {name} bytes should succeed: {err}"))?;
            if stored.text.is_some() {
                return Err(format!(
                    "{name} compressed bytes should not decode as UTF-8"
                ));
            }
            let stored_base64 = text_to_base64(name, &stored)?;

            let decoded = decode(CodecDecodeOptions {
                codec,
                input_base64: stored_base64,
            })
            .map_err(|err| format!("stored {name} bytes should decode: {err}"))?;
            if decoded.value.text.as_deref() != Some(FIXTURE) {
                return Err(format!("{name} should survive Redis round-trip"));
            }
        }

        Ok::<_, String>(())
    }
    .await;

    let cleanup_result = cleanup_keys(&state, &opened.connection_id, &keys).await;
    let close_result = close_connection(&state, &opened.connection_id).await;

    cleanup_result.unwrap_or_else(|err| {
        panic!("test keys should be removed before assertions: {err}; operation result: {operation_result:?}")
    });
    close_result.expect("connection should close before assertions");
    operation_result.expect("compression codecs should round-trip through Redis");
}
