use serial_test::serial;
use tsubasa_redis_manager_lib::error::AppError;
use tsubasa_redis_manager_lib::models::{ConnectionConfig, RedisValue};
use tsubasa_redis_manager_lib::services::connection::{
    close_connection, execute_cli, open_connection,
};
use tsubasa_redis_manager_lib::state::AppState;

fn redis_host() -> String {
    std::env::var("TEST_REDIS_HOST").unwrap_or_else(|_| "127.0.0.1".into())
}

fn redis_port() -> u16 {
    std::env::var("TEST_REDIS_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(6379)
}

fn config(readonly: bool) -> ConnectionConfig {
    ConnectionConfig {
        host: redis_host(),
        port: redis_port(),
        auth: None,
        username: None,
        db: None,
        name: Some("integration-test".into()),
        separator: Some(":".into()),
        connection_read_only: Some(readonly),
        cluster: None,
        ssl_options: None,
        ssh_options: None,
        sentinel_options: None,
    }
}

#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn open_connection_succeeds() {
    let state = AppState::default();

    let opened = open_connection(&state, config(false))
        .await
        .expect("connection should open");

    assert!(!opened.connection_id.is_empty());
    assert_eq!(opened.connection_name, "integration-test");

    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");
}

#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn execute_cli_ping_succeeds() {
    let state = AppState::default();
    let opened = open_connection(&state, config(false))
        .await
        .expect("connection should open");

    let result = execute_cli(&state, &opened.connection_id, "ping")
        .await
        .expect("ping should execute");

    assert_eq!(result.command, "ping");
    assert!(matches!(result.value, RedisValue::String(value) if value == "PONG"));

    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");
}

#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn readonly_connection_rejects_write_commands() {
    let state = AppState::default();
    let opened = open_connection(&state, config(true))
        .await
        .expect("connection should open");
    let key = format!("trdm:readonly:{}", uuid::Uuid::new_v4());

    let err = match execute_cli(&state, &opened.connection_id, &format!("set {key} value")).await {
        Ok(_) => {
            close_connection(&state, &opened.connection_id)
                .await
                .expect("readonly connection should close before cleanup");
            let writable = open_connection(&state, config(false))
                .await
                .expect("writable connection should open for cleanup");
            execute_cli(&state, &writable.connection_id, &format!("del {key}"))
                .await
                .expect("cleanup should delete unexpected readonly write");
            close_connection(&state, &writable.connection_id)
                .await
                .expect("writable cleanup connection should close");
            panic!("readonly connection allowed SET command");
        }
        Err(err) => err,
    };

    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    assert!(matches!(err, AppError::ReadonlyWriteRejected(command) if command == "set"));
}

#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn closed_connection_returns_connection_not_found() {
    let state = AppState::default();
    let opened = open_connection(&state, config(false))
        .await
        .expect("connection should open");

    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    let err = execute_cli(&state, &opened.connection_id, "ping")
        .await
        .expect_err("closed connection should not execute commands");

    assert!(matches!(err, AppError::ConnectionNotFound(id) if id == opened.connection_id));
}
