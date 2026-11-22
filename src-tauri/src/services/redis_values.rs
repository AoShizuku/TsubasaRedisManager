use base64::{engine::general_purpose, Engine as _};

use crate::models::{RedisText, RedisValue};

/// 单个值的默认截断阈值。
///
/// 一个 10 MB 的值不可能整个渲染，Monaco 也吃不消。截断发生在后端是有意的
/// （契约见 `doc/backend/ipc-contract.md`「截断契约」），截的是**原始字节**，
/// 而且尽量在服务端完成：STRING 走 `STRLEN` + `GETRANGE` 只搬一个窗口回来
/// （`text_from_window`），整值 `GET` 会让服务端把值全量复制进输出缓冲区。
/// 成员值（hash field / list item）没有范围读命令，取回后在这里截。
/// 解码路径刻意不截断（`codec.rs`），解码与截断的关系见
/// `doc/backend/codec.md`「只有后端解码才能做智能截断」。
pub const DEFAULT_VALUE_LIMIT: usize = 256 * 1024;

/// 读 pipeline 结果里的一格。
///
/// executor 统一开了 `ignore_errors`，失败的命令在这里是 `Value::ServerError`
/// 而不是让整批失败。"这一格没有" —— 命令被 ACL 拒、key 不存在、类型不匹配 ——
/// 一律用 `None` 表达，调用方不必区分原因，界面上也只需要"这一列为空"。
pub fn optional_value<T: redis::FromRedisValue>(value: Option<&redis::Value>) -> Option<T> {
    let value = value?;
    if matches!(value, redis::Value::ServerError(_) | redis::Value::Nil) {
        return None;
    }
    T::from_redis_value_ref(value).ok()
}

/// 取一格里的原始字节。集合成员、字段名、值都用它 —— 不经过 `String`，
/// 所以非 UTF-8 的成员不会在这一步丢失。
pub fn optional_bytes(value: Option<&redis::Value>) -> Option<Vec<u8>> {
    match value? {
        redis::Value::BulkString(bytes) => Some(bytes.clone()),
        redis::Value::SimpleString(text) => Some(text.as_bytes().to_vec()),
        redis::Value::Int(number) => Some(number.to_string().into_bytes()),
        redis::Value::Double(number) => Some(number.to_string().into_bytes()),
        _ => None,
    }
}

/// 字节 → `RedisText`，不截断。字段名、成员名这类短值用它。
pub fn text_from_bytes(bytes: &[u8]) -> RedisText {
    build_text(bytes, None)
}

/// 字节 → `RedisText`，超过 `limit` 时截断并带回原始总长度。
pub fn text_from_bytes_limited(bytes: &[u8], limit: usize) -> RedisText {
    if bytes.len() <= limit {
        build_text(bytes, None)
    } else {
        build_text(&bytes[..limit], Some(bytes.len() as u64))
    }
}

/// 服务端窗口取值 → `RedisText`。`bytes` 是 `GETRANGE 0 (limit-1)` 拿回的窗口，
/// `reported_len` 是同一个 pipeline 里 `STRLEN` 报的总长。
///
/// 截断判定不能只信 `STRLEN`：pipeline 不是事务，两条命令之间别的客户端可以
/// 插队写。危险的方向是值在两条命令之间**变大** —— `STRLEN` 说不足一个窗口、
/// 窗口却取满了，此时把 `total_bytes` 报成 `None` 会让编辑器当完整值放开编辑，
/// 保存即静默丢数据。所以窗口取满而 `STRLEN` 对不上时一律按截断处理，
/// 总长报已知的下界。`STRLEN` 被 ACL 单独拒掉（读作 0）走的也是这条兜底。
pub fn text_from_window(bytes: &[u8], reported_len: u64, limit: usize) -> RedisText {
    let fetched = bytes.len() as u64;
    let limit = limit as u64;
    let total = reported_len.max(fetched);
    if total > limit || (fetched == limit && reported_len != limit) {
        build_text(bytes, Some(total))
    } else {
        build_text(bytes, None)
    }
}

fn build_text(bytes: &[u8], total_bytes: Option<u64>) -> RedisText {
    match decode_utf8(bytes, total_bytes.is_some()) {
        Some(text) => RedisText {
            text: Some(text),
            base64: None,
            total_bytes,
        },
        None => RedisText {
            text: None,
            base64: Some(general_purpose::STANDARD.encode(bytes)),
            total_bytes,
        },
    }
}

/// 截断可能把一个多字节字符切成两半。`error_len() == None` 表示尾部只是**不完整**
/// 而非非法，这时退到最后一个完整字符即可 —— 否则一个纯 UTF-8 的长文本会因为
/// 截断点恰好落在汉字中间而整个退化成 base64。
fn decode_utf8(bytes: &[u8], truncated: bool) -> Option<String> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Some(text.to_string()),
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()])
                .ok()
                .map(str::to_string)
        }
        Err(_) => None,
    }
}

pub fn from_redis_value(value: redis::Value) -> RedisValue {
    match value {
        redis::Value::Nil => RedisValue::Null,
        redis::Value::Int(i) => RedisValue::Integer(i),
        redis::Value::BulkString(bytes) => match String::from_utf8(bytes) {
            Ok(value) => RedisValue::String(value),
            Err(error) => RedisValue::Object(serde_json::json!({
                "type": "binary",
                "base64": general_purpose::STANDARD.encode(error.as_bytes())
            })),
        },
        redis::Value::SimpleString(s) => RedisValue::String(s),
        redis::Value::Okay => RedisValue::String("OK".into()),
        redis::Value::Array(items) => {
            RedisValue::Array(items.into_iter().map(from_redis_value).collect())
        }
        redis::Value::Map(items) => RedisValue::Object(serde_json::json!({
            "type": "map",
            "entries": convert_map_entries(items)
        })),
        redis::Value::Attribute { data, attributes } => RedisValue::Object(serde_json::json!({
            "type": "attribute",
            "data": from_redis_value(*data),
            "attributes": convert_map_entries(attributes)
        })),
        redis::Value::Set(items) => {
            RedisValue::Array(items.into_iter().map(from_redis_value).collect())
        }
        redis::Value::Double(f) => {
            let value = if f.is_finite() {
                serde_json::json!(f)
            } else {
                serde_json::json!(f.to_string())
            };
            RedisValue::Object(serde_json::json!({
                "type": "double",
                "value": value
            }))
        }
        redis::Value::Boolean(b) => RedisValue::Object(serde_json::json!({
            "type": "boolean",
            "value": b
        })),
        redis::Value::VerbatimString { format, text } => RedisValue::Object(serde_json::json!({
            "type": "verbatimString",
            "format": format.to_string(),
            "value": text
        })),
        redis::Value::BigNumber(value) => RedisValue::Object(serde_json::json!({
            "type": "bigNumber",
            "value": value.to_string()
        })),
        redis::Value::Push { kind, data } => RedisValue::Object(serde_json::json!({
            "type": "push",
            "kind": kind.to_string(),
            "data": data.into_iter().map(from_redis_value).collect::<Vec<_>>()
        })),
        redis::Value::ServerError(error) => RedisValue::Object(serde_json::json!({
            "type": "serverError",
            "value": error.to_string()
        })),
        other => RedisValue::Object(serde_json::json!({
            "type": "unsupported",
            "value": format!("{other:?}")
        })),
    }
}

fn convert_map_entries(items: Vec<(redis::Value, redis::Value)>) -> Vec<serde_json::Value> {
    items
        .into_iter()
        .map(|(k, v)| {
            serde_json::json!({
                "key": from_redis_value(k),
                "value": from_redis_value(v)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{from_redis_value, text_from_bytes, text_from_bytes_limited, text_from_window};
    use crate::models::RedisValue;

    #[test]
    fn utf8_bytes_become_text_without_base64() {
        let value = text_from_bytes("héllo".as_bytes());
        assert_eq!(value.text.as_deref(), Some("héllo"));
        assert_eq!(value.base64, None);
        assert_eq!(value.total_bytes, None);
    }

    #[test]
    fn non_utf8_bytes_become_base64_without_text() {
        let value = text_from_bytes(&[0xff, 0x00]);
        assert_eq!(value.text, None);
        assert_eq!(value.base64.as_deref(), Some("/wA="));
    }

    #[test]
    fn short_values_are_not_truncated() {
        let value = text_from_bytes_limited(b"abc", 8);
        assert_eq!(value.text.as_deref(), Some("abc"));
        assert_eq!(value.total_bytes, None);
    }

    #[test]
    fn truncation_reports_the_original_length() {
        let value = text_from_bytes_limited(b"abcdefgh", 3);
        assert_eq!(value.text.as_deref(), Some("abc"));
        assert_eq!(value.total_bytes, Some(8));
    }

    /// 截断点落在多字节字符中间时，退到最后一个完整字符 ——
    /// 否则一段纯中文会因为切在汉字腰上而整个退化成 base64。
    #[test]
    fn truncation_backs_off_to_a_char_boundary() {
        // "中文" = 6 字节，截到 4 字节会把第二个字切两半
        let value = text_from_bytes_limited("中文".as_bytes(), 4);
        assert_eq!(value.text.as_deref(), Some("中"));
        assert_eq!(value.base64, None);
        assert_eq!(value.total_bytes, Some(6));
    }

    /// 但真正非法的字节序列不能被当成"截断的尾巴"放过去。
    #[test]
    fn truncation_still_reports_genuinely_invalid_bytes_as_binary() {
        let value = text_from_bytes_limited(&[b'a', 0xff, 0xfe, b'b', b'c'], 3);
        assert_eq!(value.text, None);
        assert!(value.base64.is_some());
        assert_eq!(value.total_bytes, Some(5));
    }

    #[test]
    fn a_window_shorter_than_the_limit_is_complete() {
        let value = text_from_window(b"abc", 3, 8);
        assert_eq!(value.text.as_deref(), Some("abc"));
        assert_eq!(value.total_bytes, None);
    }

    #[test]
    fn a_full_window_with_a_larger_reported_length_is_truncated() {
        let value = text_from_window(b"abcdefgh", 100, 8);
        assert_eq!(value.text.as_deref(), Some("abcdefgh"));
        assert_eq!(value.total_bytes, Some(100));
    }

    #[test]
    fn a_value_exactly_at_the_limit_is_complete() {
        let value = text_from_window(b"abcdefgh", 8, 8);
        assert_eq!(value.text.as_deref(), Some("abcdefgh"));
        assert_eq!(value.total_bytes, None);
    }

    /// pipeline 不是事务：STRLEN 之后、GETRANGE 之前值可能被并发写大，
    /// 这时 STRLEN 的读数已经作废。把它当真会让编辑器放开一个其实被
    /// 截断的值，保存即丢数据 —— 窗口取满而读数对不上，一律按截断处理。
    #[test]
    fn a_full_window_with_a_stale_smaller_length_is_still_truncated() {
        let value = text_from_window(b"abcdefgh", 3, 8);
        assert_eq!(value.text.as_deref(), Some("abcdefgh"));
        assert_eq!(value.total_bytes, Some(8));
    }

    /// 缺 key 时 GETRANGE 回的是空串而不是 nil —— 形状上就是"空字符串值"。
    #[test]
    fn a_missing_key_window_is_an_empty_complete_value() {
        let value = text_from_window(b"", 0, 8);
        assert_eq!(value.text.as_deref(), Some(""));
        assert_eq!(value.base64, None);
        assert_eq!(value.total_bytes, None);
    }

    /// 窗口边缘切在多字节字符中间时，与 `text_from_bytes_limited` 同款退避。
    #[test]
    fn a_truncated_window_backs_off_to_a_char_boundary() {
        // "中文" = 6 字节；窗口 4 字节取满、总长 6 → 截断，尾部退到"中"
        let value = text_from_window(&"中文".as_bytes()[..4], 6, 4);
        assert_eq!(value.text.as_deref(), Some("中"));
        assert_eq!(value.base64, None);
        assert_eq!(value.total_bytes, Some(6));
    }

    #[test]
    fn converts_bulk_string_losslessly_for_utf8() {
        let value = from_redis_value(redis::Value::BulkString(b"hello".to_vec()));
        assert!(matches!(value, RedisValue::String(s) if s == "hello"));
    }

    #[test]
    fn converts_invalid_utf8_bulk_string_to_binary_object() {
        let value = from_redis_value(redis::Value::BulkString(vec![0xff, 0x00]));
        assert!(
            matches!(value, RedisValue::Object(object) if object["type"] == "binary"
                && object["base64"] == "/wA=")
        );
    }

    #[test]
    fn converts_core_redis_values() {
        assert!(matches!(
            from_redis_value(redis::Value::Nil),
            RedisValue::Null
        ));
        assert!(matches!(
            from_redis_value(redis::Value::Int(42)),
            RedisValue::Integer(42)
        ));
        assert!(matches!(
            from_redis_value(redis::Value::SimpleString("PONG".into())),
            RedisValue::String(s) if s == "PONG"
        ));
        assert!(matches!(
            from_redis_value(redis::Value::Okay),
            RedisValue::String(s) if s == "OK"
        ));
        assert!(matches!(
            from_redis_value(redis::Value::Array(vec![redis::Value::Int(1)])),
            RedisValue::Array(items) if matches!(&items[..], [RedisValue::Integer(1)])
        ));
    }

    #[test]
    fn converts_map_to_key_value_entries() {
        let value = from_redis_value(redis::Value::Map(vec![(
            redis::Value::SimpleString("field".into()),
            redis::Value::BulkString(b"value".to_vec()),
        )]));
        assert!(
            matches!(value, RedisValue::Object(object) if object["type"] == "map"
                && object["entries"][0]["key"] == "field"
                && object["entries"][0]["value"] == "value")
        );
    }

    #[test]
    fn converts_set_to_array() {
        let value = from_redis_value(redis::Value::Set(vec![redis::Value::BulkString(
            b"a".to_vec(),
        )]));
        assert!(
            matches!(value, RedisValue::Array(items) if matches!(&items[..], [RedisValue::String(s)] if s == "a"))
        );
    }

    #[test]
    fn converts_boolean_to_typed_object() {
        let value = from_redis_value(redis::Value::Boolean(true));
        assert!(
            matches!(value, RedisValue::Object(object) if object["type"] == "boolean" && object["value"] == true)
        );
    }

    #[test]
    fn converts_double_to_typed_object() {
        let value = from_redis_value(redis::Value::Double(1.5));
        assert!(
            matches!(value, RedisValue::Object(object) if object["type"] == "double" && object["value"] == 1.5)
        );
    }

    #[test]
    fn converts_non_finite_double_to_string_value() {
        let value = from_redis_value(redis::Value::Double(f64::NAN));
        assert!(
            matches!(value, RedisValue::Object(object) if object["type"] == "double" && object["value"] == "NaN")
        );

        let value = from_redis_value(redis::Value::Double(f64::INFINITY));
        assert!(
            matches!(value, RedisValue::Object(object) if object["type"] == "double" && object["value"] == "inf")
        );

        let value = from_redis_value(redis::Value::Double(f64::NEG_INFINITY));
        assert!(
            matches!(value, RedisValue::Object(object) if object["type"] == "double" && object["value"] == "-inf")
        );
    }

    #[test]
    fn converts_verbatim_string_to_typed_object() {
        let value = from_redis_value(redis::Value::VerbatimString {
            format: redis::VerbatimFormat::Text,
            text: "hello".into(),
        });
        assert!(
            matches!(value, RedisValue::Object(object) if object["type"] == "verbatimString"
                && object["format"] == "txt"
                && object["value"] == "hello")
        );
    }

    #[test]
    fn converts_attribute_to_typed_object() {
        let value = from_redis_value(redis::Value::Attribute {
            data: Box::new(redis::Value::BulkString(b"payload".to_vec())),
            attributes: vec![(
                redis::Value::SimpleString("ttl".into()),
                redis::Value::Int(10),
            )],
        });
        assert!(
            matches!(value, RedisValue::Object(object) if object["type"] == "attribute"
                && object["data"] == "payload"
                && object["attributes"][0]["key"] == "ttl"
                && object["attributes"][0]["value"] == 10)
        );
    }

    #[test]
    fn converts_big_number_to_typed_object() {
        let big_number =
            redis::parse_redis_value(b"(3492890328409238509324850943850943825024385\r\n").unwrap();
        let value = from_redis_value(big_number);
        assert!(
            matches!(value, RedisValue::Object(object) if object["type"] == "bigNumber"
                && object["value"] == "3492890328409238509324850943850943825024385")
        );
    }

    #[test]
    fn converts_push_to_typed_object() {
        let value = from_redis_value(redis::Value::Push {
            kind: redis::PushKind::Message,
            data: vec![redis::Value::SimpleString("channel".into())],
        });
        assert!(
            matches!(value, RedisValue::Object(object) if object["type"] == "push"
                && object["kind"] == "message"
                && object["data"][0] == "channel")
        );
    }

    #[test]
    fn converts_server_error_to_typed_object() {
        let server_error = redis::parse_redis_value(b"!21\r\nSYNTAX invalid syntax\r\n").unwrap();
        let value = from_redis_value(server_error);
        assert!(
            matches!(value, RedisValue::Object(object) if object["type"] == "serverError"
                && object["value"].as_str().is_some_and(|value| value.contains("invalid syntax")))
        );
    }
}
