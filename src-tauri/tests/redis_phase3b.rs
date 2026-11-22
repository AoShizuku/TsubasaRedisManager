use serial_test::serial;
use tsubasa_redis_manager_lib::models::{ConnectionConfig, RedisValue};
use tsubasa_redis_manager_lib::services::connection::{
    close_connection, execute_cli, open_connection,
};
use tsubasa_redis_manager_lib::state::AppState;

const LOCAL_VDS_ENV_HELP: &str = "Copy scripts/ci/local-vds.env.example.ps1 to scripts/ci/local-vds.env.ps1 for ignored VDS E2E tests.";

fn optional_env_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn redis_host() -> String {
    optional_env_string("PHASE3B_REDIS_HOST")
        .or_else(|| optional_env_string("PHASE2_HOST"))
        .unwrap_or_else(|| {
            panic!(
                "PHASE3B_REDIS_HOST or PHASE2_HOST must be set to run ignored VDS E2E tests. {LOCAL_VDS_ENV_HELP}"
            );
        })
}

fn config() -> ConnectionConfig {
    ConnectionConfig {
        host: redis_host(),
        port: std::env::var("PHASE3B_REDIS_PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(16379),
        auth: None,
        username: None,
        db: Some(14),
        name: Some("phase3b".into()),
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
fn phase3b_host_requires_explicit_env_when_missing() {
    let _guard = EnvGuard::new(&["PHASE3B_REDIS_HOST", "PHASE2_HOST"]);
    std::env::remove_var("PHASE3B_REDIS_HOST");
    std::env::remove_var("PHASE2_HOST");

    let message = catch_expected_panic(config);

    assert!(message.contains("PHASE3B_REDIS_HOST or PHASE2_HOST"));
    assert!(message.contains("scripts/ci/local-vds.env.example.ps1"));
}

#[test]
#[serial]
fn phase3b_host_requires_explicit_env_when_blank() {
    let _guard = EnvGuard::new(&["PHASE3B_REDIS_HOST", "PHASE2_HOST"]);
    std::env::set_var("PHASE3B_REDIS_HOST", " ");
    std::env::set_var("PHASE2_HOST", "");

    let message = catch_expected_panic(config);

    assert!(message.contains("PHASE3B_REDIS_HOST or PHASE2_HOST"));
    assert!(message.contains("scripts/ci/local-vds.env.ps1"));
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn command_file_import_fixture_executes_valid_commands() {
    // Command-file import fixture prerequisite smoke: direct execute_cli verifies VDS
    // command execution; actual file-task runtime verification is covered later.
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let key = format!("phase3b:cmd:{token}");

    let operation_result = async {
        execute_cli(&state, &opened.connection_id, &format!("SET {key} value"))
            .await
            .map_err(|err| format!("SET should execute: {err}"))?;
        execute_cli(&state, &opened.connection_id, &format!("GET {key}"))
            .await
            .map_err(|err| format!("GET should execute: {err}"))
    }
    .await;
    let cleanup_result = execute_cli(&state, &opened.connection_id, &format!("DEL {key}")).await;
    let close_result = close_connection(&state, &opened.connection_id).await;

    cleanup_result.expect("cleanup should delete command fixture key");
    close_result.expect("connection should close before assertions");

    let result = operation_result.expect("command fixture should execute valid commands");
    assert!(matches!(result.value, RedisValue::String(value) if value == "value"));
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn monitor_vds_fixture_can_observe_second_connection_command_prerequisite() {
    let state = AppState::default();
    let first = open_connection(&state, config())
        .await
        .expect("first connection should open");
    let second_result = open_connection(&state, config()).await;
    let second = match second_result {
        Ok(second) => second,
        Err(err) => {
            close_connection(&state, &first.connection_id)
                .await
                .expect("first connection should close after second open failure");
            panic!("second connection should open: {err}");
        }
    };
    let key = format!("phase3b:monitor:{}", uuid::Uuid::new_v4());

    let operation_result = async {
        execute_cli(&state, &second.connection_id, &format!("SET {key} value"))
            .await
            .map_err(|err| format!("second connection should write monitored command: {err}"))
    }
    .await;
    let cleanup_result = execute_cli(&state, &second.connection_id, &format!("DEL {key}")).await;
    let first_close_result = close_connection(&state, &first.connection_id).await;
    let second_close_result = close_connection(&state, &second.connection_id).await;

    first_close_result.expect("first connection should close");
    second_close_result.expect("second connection should close");
    operation_result.expect("monitor prerequisite commands should execute");
    cleanup_result.expect("cleanup should delete monitored key");
}

#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn pubsub_vds_fixture_can_publish_to_unique_channel_prerequisite() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let channel = format!("phase3b:channel:{}", uuid::Uuid::new_v4());

    let operation_result = execute_cli(
        &state,
        &opened.connection_id,
        &format!("PUBLISH {channel} hello"),
    )
    .await;
    let close_result = close_connection(&state, &opened.connection_id).await;

    close_result.expect("connection should close before assertions");
    operation_result.expect("publish command should execute");
}
