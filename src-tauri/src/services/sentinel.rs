use crate::error::{AppError, AppResult};
use redis::FromRedisValue;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentinelMaster {
    pub host: String,
    pub port: u16,
}

pub fn parse_master_addr(value: redis::Value) -> AppResult<SentinelMaster> {
    let items: Vec<String> = Vec::<String>::from_redis_value(value)
        .map_err(|e| AppError::RedisSentinelFailed(e.to_string()))?;
    if items.len() != 2 {
        return Err(AppError::RedisSentinelFailed(
            "Sentinel master address was not returned".into(),
        ));
    }
    let port = items[1].parse::<u16>().map_err(|e| {
        AppError::RedisSentinelFailed(format!("Invalid Sentinel master port {}: {e}", items[1]))
    })?;

    Ok(SentinelMaster {
        host: items[0].clone(),
        port,
    })
}

pub async fn resolve_master<C>(sentinel: &mut C, master_name: &str) -> AppResult<SentinelMaster>
where
    C: redis::aio::ConnectionLike,
{
    let value = redis::cmd("SENTINEL")
        .arg("get-master-addr-by-name")
        .arg(master_name)
        .query_async::<redis::Value>(sentinel)
        .await
        .map_err(|e| AppError::RedisSentinelFailed(e.to_string()))?;

    parse_master_addr(value)
}

#[cfg(test)]
mod tests {
    use super::parse_master_addr;

    #[test]
    fn parses_master_addr_array() {
        let value = redis::Value::Array(vec![
            redis::Value::BulkString(b"10.0.0.10".to_vec()),
            redis::Value::BulkString(b"6379".to_vec()),
        ]);

        let master = parse_master_addr(value).unwrap();

        assert_eq!(master.host, "10.0.0.10");
        assert_eq!(master.port, 6379);
    }

    #[test]
    fn rejects_nil_with_sentinel_failed_payload_code() {
        let err = parse_master_addr(redis::Value::Nil).unwrap_err();

        assert_eq!(err.payload().code, "redis.sentinel_failed");
    }
}
