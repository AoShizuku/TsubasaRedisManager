//! 语义化取值层的端到端测试。
//!
//! 这一层的纯函数部分（回复解析、UTF-8 截断边界）在 `services/values.rs` 与
//! `services/redis_values.rs` 里有单元测试，但那些测试喂的是手工构造的
//! `redis::Value`。真正的风险在于**真实服务端回什么形状**——尤其是分页：
//! 端点算错、区间方向反了、边界差一，单元测试一个都发现不了。
//!
//! 需要一个可写的单机 Redis：`TEST_REDIS_HOST` / `TEST_REDIS_PORT`。

use serial_test::serial;
use tsubasa_redis_manager_lib::error::AppError;
use tsubasa_redis_manager_lib::models::{ConnectionConfig, RedisValue};
use tsubasa_redis_manager_lib::services::connection::{
    close_connection, execute_cli, get_key_meta, open_connection,
};
use tsubasa_redis_manager_lib::services::values::{
    add_hll_elements, delete_keys, get_full_value, get_hash_entries, get_hll_count, get_list_range,
    get_stream_entries, get_string_value, get_zset_range, inspect_hll, push_list_item, rename_key,
    set_hash_field, set_string_value, ValueRef,
};
use tsubasa_redis_manager_lib::state::AppState;

const ENV_HELP: &str =
    "Set TEST_REDIS_HOST and TEST_REDIS_PORT to run the ignored value-layer E2E tests.";

fn config() -> ConnectionConfig {
    let host = std::env::var("TEST_REDIS_HOST")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("TEST_REDIS_HOST must be set. {ENV_HELP}"));
    let port = std::env::var("TEST_REDIS_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(6379);

    ConnectionConfig {
        host,
        port,
        auth: None,
        username: None,
        db: Some(0),
        name: Some("value-layer e2e".into()),
        separator: Some(":".into()),
        connection_read_only: Some(false),
        cluster: Some(false),
        ssl_options: None,
        ssh_options: None,
        sentinel_options: None,
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 保存必须保留毫秒级的绝对过期时间，不能清掉 TTL 或重新开始倒计时。
/// 测试环境是 Redis 7，用 PEXPIRETIME 精确比较，避免依赖 sleep 与时间容差。
#[tokio::test]
#[serial]
#[ignore = "requires a local Redis 7+ instance"]
async fn string_save_preserves_expiration_and_bytes() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let keys: Vec<String> = ["expiring", "persistent", "missing"]
        .iter()
        .map(|kind| format!("values:string-save:{kind}:{token}"))
        .collect();
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_millis() as i64
        + 600_123;

    let result = async {
        for (index, key) in keys.iter().enumerate() {
            if index < 2 {
                execute_cli(&state, &opened.connection_id, &format!("SET {key} original"))
                    .await
                    .map_err(|err| format!("seed should succeed: {err:?}"))?;
            }
            if index == 0 {
                execute_cli(
                    &state,
                    &opened.connection_id,
                    &format!("PEXPIREAT {key} {expires_at}"),
                )
                .await
                .map_err(|err| format!("setting expiration should succeed: {err:?}"))?;
            }
            let expected_expiration = if index == 0 { expires_at } else { -1 };
            // 反复保存：二进制、空值、UTF-8；覆盖新建与覆盖已有值。
            for bytes in [b"\x00\xff\x80\r\n".as_slice(), b"", "新值".as_bytes()] {
                set_string_value(&state, &opened.connection_id, key, &base64_encode(bytes))
                    .await
                    .map_err(|err| format!("saving {key} should succeed: {err:?}"))?;
                let stored = get_string_value(&state, &opened.connection_id, key)
                    .await
                    .map_err(|err| format!("reading {key} should succeed: {err:?}"))?;
                let stored_base64 = stored
                    .text
                    .map(|text| base64_encode(text.as_bytes()))
                    .or(stored.base64);
                if stored_base64 != Some(base64_encode(bytes)) || stored.total_bytes.is_some() {
                    return Err(format!("saved bytes should round-trip exactly for {key}"));
                }
                let expiration = execute_cli(
                    &state,
                    &opened.connection_id,
                    &format!("PEXPIRETIME {key}"),
                )
                .await
                .map_err(|err| format!("reading expiration should succeed: {err:?}"))?;
                if !matches!(expiration.value, RedisValue::Integer(value) if value == expected_expiration)
                {
                    return Err(format!(
                        "{key}: expected expiration {expected_expiration}, got {:?}",
                        expiration.value
                    ));
                }
            }
        }
        Ok::<_, String>(())
    }
    .await;

    let cleanup = delete_keys(&state, &opened.connection_id, &keys).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");
    cleanup.expect("test keys should be removed");
    result.expect("string saves should preserve expiration and bytes");
}

/// Stream 的分页方向。
///
/// 端点组装从前端搬到后端时顺带反了向——`XREVRANGE` 的区间是 end 在前、
/// start 在后。推理上是对的，但翻页这种东西推理对不等于跑对，
/// 而且要翻过第一页才会暴露：第一页两个方向都是 `- +`，看不出差别。
#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn stream_paging_walks_every_entry_exactly_once_in_both_directions() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let key = format!("values:stream:{token}");
    const TOTAL: usize = 250;
    const PAGE: usize = 100;

    let result = async {
        for index in 0..TOTAL {
            execute_cli(
                &state,
                &opened.connection_id,
                &format!("XADD {key} * seq {index}"),
            )
            .await
            .map_err(|err| format!("seeding entry {index} should succeed: {err:?}"))?;
        }

        for reversed in [false, true] {
            let mut seen: Vec<String> = Vec::new();
            loop {
                // 与 StreamViewer 完全相同的续页算法
                let last = seen.last().cloned();
                let start = match (&last, reversed) {
                    (Some(id), false) => format!("({id}"),
                    _ => "-".to_string(),
                };
                let end = match (&last, reversed) {
                    (Some(id), true) => format!("({id}"),
                    _ => "+".to_string(),
                };

                let page = get_stream_entries(
                    &state,
                    &opened.connection_id,
                    &key,
                    &start,
                    &end,
                    Some(PAGE),
                    reversed,
                )
                .await
                .map_err(|err| format!("reading a page should succeed: {err:?}"))?;

                if page.total != TOTAL as u64 {
                    return Err(format!("expected a total of {TOTAL}, got {}", page.total));
                }
                let count = page.entries.len();
                seen.extend(page.entries.into_iter().map(|entry| entry.id));
                if count < PAGE {
                    break;
                }
                if seen.len() > TOTAL {
                    return Err(format!(
                        "reversed={reversed}: paging overran, saw {} entries",
                        seen.len()
                    ));
                }
            }

            if seen.len() != TOTAL {
                return Err(format!(
                    "reversed={reversed}: expected {TOTAL} entries, saw {}",
                    seen.len()
                ));
            }

            let mut unique = seen.clone();
            unique.sort();
            unique.dedup();
            if unique.len() != TOTAL {
                return Err(format!(
                    "reversed={reversed}: paging returned duplicates ({} unique of {})",
                    unique.len(),
                    seen.len()
                ));
            }

            // ID 是单调的，方向必须与请求一致；错了说明端点接反了
            let ordered = if reversed {
                seen.windows(2).all(|pair| pair[0] > pair[1])
            } else {
                seen.windows(2).all(|pair| pair[0] < pair[1])
            };
            if !ordered {
                return Err(format!(
                    "reversed={reversed}: entries came back out of order"
                ));
            }
        }

        Ok::<_, String>(())
    }
    .await;

    let cleanup = delete_keys(&state, &opened.connection_id, &[key]).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");
    cleanup.expect("test key should be removed");
    result.expect("stream paging should cover every entry in both directions");
}

/// 大值截断，以及截断点落在多字节字符中间时退到完整字符。
///
/// `totalBytes` 是界面禁止保存的依据——漏了它，用户会把截断后的值存回去，
/// 静默丢掉后面的部分。
#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn oversized_values_are_truncated_and_report_the_original_length() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let ascii_key = format!("values:big:ascii:{token}");
    let cjk_key = format!("values:big:cjk:{token}");
    // 后端阈值是 256 KiB
    const LIMIT: usize = 256 * 1024;

    let result = async {
        let ascii = "x".repeat(LIMIT + 4096);
        set_string_value(
            &state,
            &opened.connection_id,
            &ascii_key,
            &base64_encode(ascii.as_bytes()),
        )
        .await
        .map_err(|err| format!("writing the large ascii value should succeed: {err:?}"))?;

        let stored = get_string_value(&state, &opened.connection_id, &ascii_key)
            .await
            .map_err(|err| format!("reading it back should succeed: {err:?}"))?;
        if stored.total_bytes != Some(ascii.len() as u64) {
            return Err(format!(
                "expected totalBytes {}, got {:?}",
                ascii.len(),
                stored.total_bytes
            ));
        }
        let text = stored.text.ok_or("an ascii value should decode as text")?;
        if text.len() != LIMIT {
            return Err(format!(
                "expected {LIMIT} bytes after truncation, got {}",
                text.len()
            ));
        }

        // 每个汉字 3 字节，256 KiB 不是 3 的整数倍，所以截断点必然落在字符中间
        let cjk = "中".repeat(LIMIT); // 远超上限
        set_string_value(
            &state,
            &opened.connection_id,
            &cjk_key,
            &base64_encode(cjk.as_bytes()),
        )
        .await
        .map_err(|err| format!("writing the large cjk value should succeed: {err:?}"))?;

        let stored = get_string_value(&state, &opened.connection_id, &cjk_key)
            .await
            .map_err(|err| format!("reading it back should succeed: {err:?}"))?;
        if stored.total_bytes != Some(cjk.len() as u64) {
            return Err(format!(
                "expected totalBytes {}, got {:?}",
                cjk.len(),
                stored.total_bytes
            ));
        }
        // 关键：必须仍然是文本。退不回完整字符的话整段会退化成 base64，
        // 界面上一段中文就会变成十六进制。
        let text = stored.text.ok_or(
            "a truncated cjk value must back off to a char boundary, not fall back to base64",
        )?;
        if !text.chars().all(|ch| ch == '中') {
            return Err("truncated cjk text contains a broken character".to_string());
        }
        if text.len() % 3 != 0 {
            return Err(format!(
                "truncation did not land on a char boundary: {} bytes",
                text.len()
            ));
        }

        Ok::<_, String>(())
    }
    .await;

    let cleanup = delete_keys(&state, &opened.connection_id, &[ascii_key, cjk_key]).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");
    cleanup.expect("test keys should be removed");
    result.expect("large values should truncate and report their real length");
}

/// ZSet 的 `inf` / `-inf` 是合法分数，而 JSON 装不下它们——
/// 这正是分数按字符串回传而不是 f64 的原因。
#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn zset_scores_survive_infinity() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let key = format!("values:zset:{token}");

    let result = async {
        for (score, member) in [("-inf", "lowest"), ("1.5", "middle"), ("+inf", "highest")] {
            execute_cli(
                &state,
                &opened.connection_id,
                &format!("ZADD {key} {score} {member}"),
            )
            .await
            .map_err(|err| format!("seeding {member} should succeed: {err:?}"))?;
        }

        let page = get_zset_range(&state, &opened.connection_id, &key, 0, -1, false)
            .await
            .map_err(|err| format!("reading the zset should succeed: {err:?}"))?;

        if page.total != 3 {
            return Err(format!("expected a total of 3, got {}", page.total));
        }
        let rows = page
            .members
            .iter()
            .map(|entry| {
                (
                    entry.member.text.clone().unwrap_or_default(),
                    entry.score.clone(),
                )
            })
            .collect::<Vec<_>>();

        // 分数序：-inf 最小，+inf 最大
        let order = rows.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>();
        if order != vec!["lowest", "middle", "highest"] {
            return Err(format!("scores did not order correctly: {order:?}"));
        }
        // 无穷必须原样带回来，不能变成 null / 0 / 一个巨大的数
        if !rows[0].1.contains("inf") || !rows[2].1.contains("inf") {
            return Err(format!("infinite scores were mangled: {rows:?}"));
        }

        // 元信息也要能读到这个 key
        let meta = get_key_meta(&state, &opened.connection_id, &key)
            .await
            .map_err(|err| format!("reading meta should succeed: {err:?}"))?;
        if meta.key_type.as_deref() != Some("zset") || meta.size != Some(3) {
            return Err(format!(
                "unexpected meta: {:?} / {:?}",
                meta.key_type, meta.size
            ));
        }

        Ok::<_, String>(())
    }
    .await;

    let cleanup = delete_keys(&state, &opened.connection_id, &[key]).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");
    cleanup.expect("test key should be removed");
    result.expect("infinite zset scores should round-trip");
}

/// 凭据错误必须**快速**失败。
///
/// redis-rs 把 `AuthenticationFailed` 当作可重试错误，默认重试 6 次：
/// 密码填错要退避满 6 轮才报错，实测 9.6 秒。用户点了"连接"之后干等十秒、
/// 界面上还没有任何反馈，只会以为是自己网络的问题 —— 而重试多少次
/// 密码都不会变对。`REDIS_CONNECTION_RETRIES` 把它压到了 1 次。
///
/// 阈值取 3 秒：真实值是百毫秒级，留足 CI 抖动的余量，
/// 但仍然能挡住"有人把重试次数调回默认"这种回退。
#[tokio::test]
#[serial]
#[ignore = "requires a password-protected Redis"]
async fn wrong_credentials_fail_fast_instead_of_retrying() {
    use std::time::Instant;

    let port = std::env::var("TEST_REDIS_AUTH_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("TEST_REDIS_AUTH_PORT must be set. {ENV_HELP}"));

    let mut wrong = config();
    wrong.port = port;
    wrong.auth = Some("definitely-not-the-password".into());

    let started = Instant::now();
    let error = open_connection(&AppState::default(), wrong)
        .await
        .expect_err("a wrong password must not connect");
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "a wrong password took {elapsed:?} to fail; the connection retry count has probably \
         drifted back to the redis-rs default of 6"
    );
    // 报出来的必须是"认证失败"，不能是超时——后者会把用户引到错误的方向
    let text = format!("{error:?}").to_lowercase();
    assert!(
        text.contains("auth") || text.contains("password"),
        "the error should name the credentials, got: {error:?}"
    );
}

/// 只读会话必须自己报告只读，而且真的拦得住写命令。
///
/// `ConnectionOpened.readonly` 是前端唯一的只读依据 —— 界面照它禁用编辑控件。
/// 它一旦和 `writable_runtime` 判的值不是同一个，就会出现"按钮能点、命令被拒"，
/// 或者更糟的反向：**界面以为可写，守卫也放行**。
///
/// 曾经就是后者：前端不看这个字段（当时压根没有），而是自己从连接配置推断，
/// 于是把连接改成只读之后，那个在改动之前建立的会话仍然放行 RENAME ——
/// 用户在生产库上重命名成功了才发现只读没生效。
#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn readonly_sessions_report_themselves_and_reject_writes() {
    let state = AppState::default();
    let token = uuid::Uuid::new_v4();
    let key = format!("values:readonly:{token}");

    // 种数据要用可写会话——只读会话连准备工作都做不了，这本身就是它该有的样子
    let writable = open_connection(&state, config())
        .await
        .expect("the writable connection should open");
    assert!(
        !writable.readonly,
        "a connection configured as writable must not report itself readonly"
    );
    set_string_value(&state, &writable.connection_id, &key, &base64_encode(b"v"))
        .await
        .expect("seeding through the writable session should succeed");

    let mut locked = config();
    locked.connection_read_only = Some(true);
    let readonly = open_connection(&state, locked)
        .await
        .expect("the readonly connection should open");

    let result = async {
        // 契约：会话自报只读，前端据此上锁
        if !readonly.readonly {
            return Err("a readonly connection must report readonly = true".to_string());
        }

        // 守卫：写命令被拒，且拒的理由要说得清楚
        let moved = format!("{key}:moved");
        match rename_key(&state, &readonly.connection_id, &key, &moved).await {
            Err(AppError::ReadonlyWriteRejected(intent)) if intent == "RENAME" => {}
            Err(other) => return Err(format!("RENAME rejected for the wrong reason: {other:?}")),
            Ok(()) => return Err("a readonly session renamed a key".to_string()),
        }
        if set_string_value(&state, &readonly.connection_id, &key, &base64_encode(b"x"))
            .await
            .is_ok()
        {
            return Err("a readonly session overwrote a value".to_string());
        }

        // 只读不是"什么都不能做"：读必须照常
        let stored = get_string_value(&state, &readonly.connection_id, &key)
            .await
            .map_err(|err| format!("a readonly session must still read: {err:?}"))?;
        if stored.text.as_deref() != Some("v") {
            return Err(format!("the value should be untouched, got {stored:?}"));
        }

        Ok::<_, String>(())
    }
    .await;

    let cleanup = delete_keys(&state, &writable.connection_id, &[key]).await;
    close_connection(&state, &readonly.connection_id)
        .await
        .expect("the readonly connection should close");
    close_connection(&state, &writable.connection_id)
        .await
        .expect("the writable connection should close");
    cleanup.expect("test key should be removed");
    result.expect("a readonly session should report itself and reject writes");
}

/// 重取要真的跨过分页的截断，而且要按**原始字节**定位。
///
/// `values.rs` 的单元测试只验得了命令拼装（`full_value_command` 里有没有 `HGET`、
/// field 有没有被 base64 解回字节）。这里问的是另一个问题：**真实服务端认不认**。
///
/// 非 UTF-8 的 field 名在界面上渲染成 `1f 8b 08` 那样的十六进制串，拿那串文本
/// 去发命令的话，服务端当然一个字段都找不到，而且是**静默**找不到。
/// 这条测试把两边都钉住：原始字节能取到，十六进制渲染取不到。
/// 见 `doc/backend/ipc-contract.md`「完整值重取」。
#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn full_value_refetches_past_the_page_limit_and_locates_by_raw_bytes() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let key = format!("values:full:hash:{token}");
    // 分页里集合成员的阈值是 64 KiB
    const MEMBER_LIMIT: usize = 64 * 1024;
    // gzip 的魔数：既是合法的 field 名，又必然不是 UTF-8
    const FIELD: &[u8] = &[0x1f, 0x8b, 0x08];

    let result = async {
        let big = "x".repeat(MEMBER_LIMIT + 4096);
        set_hash_field(
            &state,
            &opened.connection_id,
            &key,
            &base64_encode(FIELD),
            &base64_encode(big.as_bytes()),
        )
        .await
        .map_err(|err| format!("seeding the hash field should succeed: {err:?}"))?;

        // 分页这一趟必须是截断的，否则后面的重取就没有意义
        let page = get_hash_entries(&state, &opened.connection_id, &key, "0", None, None)
            .await
            .map_err(|err| format!("reading the hash page should succeed: {err:?}"))?;
        let entry = page
            .entries
            .first()
            .ok_or_else(|| "the hash page should contain the seeded field".to_string())?;
        if entry.value.total_bytes != Some(big.len() as u64) {
            return Err(format!(
                "the page should report the full length {}, got {:?}",
                big.len(),
                entry.value.total_bytes
            ));
        }
        // field 名不截断（`short_text`），所以它能当定位符 —— 这是重取成立的前提
        if entry.field.base64.as_deref() != Some(base64_encode(FIELD).as_str()) {
            return Err(format!(
                "the field name should survive as bytes, got {:?}",
                entry.field
            ));
        }

        let full = get_full_value(
            &state,
            &opened.connection_id,
            &ValueRef::HashField {
                key: key.clone(),
                field_base64: base64_encode(FIELD),
            },
        )
        .await
        .map_err(|err| format!("refetching by raw field bytes should succeed: {err:?}"))?;
        if full.total_bytes.is_some() {
            return Err(format!(
                "the refetched value must not be truncated, got {:?}",
                full.total_bytes
            ));
        }
        if full.text.as_deref() != Some(big.as_str()) {
            return Err("the refetched value should be the whole thing".to_string());
        }

        // 界面上显示的那串十六进制拿去 HGET 什么也取不到 —— 静默地取不到，
        // 这正是 v0.4 之前 ValueDrawer 与 HashViewer 的写路径在做的事
        let by_rendering = get_full_value(
            &state,
            &opened.connection_id,
            &ValueRef::HashField {
                key: key.clone(),
                field_base64: base64_encode(b"1f 8b 08"),
            },
        )
        .await
        .map_err(|err| format!("a miss should be an empty value, not an error: {err:?}"))?;
        if by_rendering.text.is_some() || by_rendering.base64.is_some() {
            return Err(format!(
                "the hex rendering must not resolve to a field, got {by_rendering:?}"
            ));
        }

        Ok::<_, String>(())
    }
    .await;

    let cleanup = delete_keys(&state, &opened.connection_id, &[key]).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("the connection should close");
    cleanup.expect("test key should be removed");
    result.expect("refetching a hash field should cross the page limit and match on bytes");
}

/// STRING 与 LIST 的定位符。
///
/// LIST 那一条问的是下标口径：`ListPage.start` 加行号得到的是**整个列表**里的
/// 绝对下标，`LINDEX` 吃的也是它。翻过第一页才看得出差别，所以这里读第二页。
#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn full_value_locates_a_string_and_a_list_item() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let string_key = format!("values:full:string:{token}");
    let list_key = format!("values:full:list:{token}");
    // STRING 的分页阈值是 256 KiB
    const STRING_LIMIT: usize = 256 * 1024;
    const ITEMS: usize = 40;
    const PAGE: i64 = 25;

    let result = async {
        let big = "y".repeat(STRING_LIMIT + 4096);
        set_string_value(
            &state,
            &opened.connection_id,
            &string_key,
            &base64_encode(big.as_bytes()),
        )
        .await
        .map_err(|err| format!("seeding the large string should succeed: {err:?}"))?;

        let truncated = get_string_value(&state, &opened.connection_id, &string_key)
            .await
            .map_err(|err| format!("reading it back should succeed: {err:?}"))?;
        if truncated.total_bytes != Some(big.len() as u64) {
            return Err("the paged read should report truncation".to_string());
        }

        let full = get_full_value(
            &state,
            &opened.connection_id,
            &ValueRef::String {
                key: string_key.clone(),
            },
        )
        .await
        .map_err(|err| format!("refetching the string should succeed: {err:?}"))?;
        if full.total_bytes.is_some() || full.text.as_deref() != Some(big.as_str()) {
            return Err("the refetched string should be whole".to_string());
        }

        for index in 0..ITEMS {
            push_list_item(
                &state,
                &opened.connection_id,
                &list_key,
                &base64_encode(format!("item-{index}").as_bytes()),
                true,
            )
            .await
            .map_err(|err| format!("seeding item {index} should succeed: {err:?}"))?;
        }

        // 第二页：start 非零，行号与绝对下标才分得开
        let page = get_list_range(&state, &opened.connection_id, &list_key, PAGE, PAGE * 2 - 1)
            .await
            .map_err(|err| format!("reading the second page should succeed: {err:?}"))?;
        if page.start != PAGE {
            return Err(format!(
                "the page should start at {PAGE}, got {}",
                page.start
            ));
        }
        let row = 3usize;
        let absolute = page.start + row as i64;
        let expected = page
            .items
            .get(row)
            .and_then(|item| item.text.clone())
            .ok_or_else(|| "the page should have that row".to_string())?;

        let item = get_full_value(
            &state,
            &opened.connection_id,
            &ValueRef::ListIndex {
                key: list_key.clone(),
                index: absolute,
            },
        )
        .await
        .map_err(|err| format!("refetching by absolute index should succeed: {err:?}"))?;
        if item.text.as_deref() != Some(expected.as_str()) {
            return Err(format!(
                "index {absolute} should be {expected:?}, got {:?}",
                item.text
            ));
        }

        // 越界不是错误，是空值 —— 界面按"这一格没有"处理
        let past_end = get_full_value(
            &state,
            &opened.connection_id,
            &ValueRef::ListIndex {
                key: list_key.clone(),
                index: ITEMS as i64 + 10,
            },
        )
        .await
        .map_err(|err| format!("an out-of-range index should be empty, not an error: {err:?}"))?;
        if past_end.text.is_some() || past_end.base64.is_some() {
            return Err(format!(
                "an out-of-range index should be empty, got {past_end:?}"
            ));
        }

        Ok::<_, String>(())
    }
    .await;

    let cleanup = delete_keys(&state, &opened.connection_id, &[string_key, list_key]).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("the connection should close");
    cleanup.expect("test keys should be removed");
    result.expect("string and list locators should resolve against a real server");
}

fn base64_decode(value: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .expect("test payload should be valid base64")
}

/// HLL 全链路：识别（GETRANGE 头部探针）→ 计数 → PFADD → sparse 转 dense → 结构解析。
///
/// `services/hll.rs` 的单元测试喂的是手工构造的字节；这里验证的是**真实 Redis
/// 写出的 HLL**与解析器的一致性——头部布局、sparse 操作码、超过
/// `hll-sparse-max-bytes` 后转 dense 的时机、6-bit 打包，全都是服务端行为。
#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn hyperloglog_full_path_detects_counts_and_inspects() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let key = format!("values:hll:{token}");

    let result = async {
        execute_cli(
            &state,
            &opened.connection_id,
            &format!("PFADD {key} alpha beta gamma"),
        )
        .await
        .map_err(|err| format!("seeding should succeed: {err:?}"))?;

        // 识别：TYPE 是 string（子形态不升格），hll 元信息标注 sparse
        let meta = get_key_meta(&state, &opened.connection_id, &key)
            .await
            .map_err(|err| format!("meta should load: {err:?}"))?;
        if meta.key_type.as_deref() != Some("string") {
            return Err(format!("TYPE should stay string, got {:?}", meta.key_type));
        }
        match meta.hll.as_ref().map(|hll| hll.encoding.as_str()) {
            Some("sparse") => {}
            other => return Err(format!("a fresh HLL should be sparse, got {other:?}")),
        }

        let count = get_hll_count(&state, &opened.connection_id, &key)
            .await
            .map_err(|err| format!("PFCOUNT should succeed: {err:?}"))?;
        if count != 3 {
            return Err(format!("3 distinct elements should count 3, got {count}"));
        }

        // PFADD 语义化命令：新元素改变基数，重复元素不变
        let changed = add_hll_elements(
            &state,
            &opened.connection_id,
            &key,
            &[base64_encode(b"delta")],
        )
        .await
        .map_err(|err| format!("PFADD should succeed: {err:?}"))?;
        if !changed {
            return Err("adding a new element should change the cardinality".into());
        }
        let changed_again = add_hll_elements(
            &state,
            &opened.connection_id,
            &key,
            &[base64_encode(b"delta")],
        )
        .await
        .map_err(|err| format!("repeated PFADD should succeed: {err:?}"))?;
        if changed_again {
            return Err("re-adding the same element should not change the cardinality".into());
        }

        // sparse 的结构：操作码流展开成完整的 16,384 个寄存器
        let sparse = inspect_hll(&state, &opened.connection_id, &key)
            .await
            .map_err(|err| format!("inspect should succeed: {err:?}"))?;
        let header = sparse.header.ok_or("the sparse header should parse")?;
        if header.encoding != "sparse" {
            return Err(format!("expected sparse, got {}", header.encoding));
        }
        if sparse.valid_registers != 16384 || sparse.expected_bytes.is_some() {
            return Err(format!(
                "a valid sparse HLL should expand fully, got valid={} expected={:?}",
                sparse.valid_registers, sparse.expected_bytes
            ));
        }
        let registers = base64_decode(
            sparse
                .registers_base64
                .as_deref()
                .ok_or("sparse registers should expand")?,
        );
        if registers.len() != 16384 {
            return Err(format!("expected 16384 registers, got {}", registers.len()));
        }
        let nonzero = registers.iter().filter(|value| **value > 0).count();
        if !(1..=4).contains(&nonzero) {
            return Err(format!(
                "4 distinct elements should light up 1..=4 registers, got {nonzero}"
            ));
        }

        // 灌 5,000 个元素：超过 hll-sparse-max-bytes（默认 3000）必然转 dense
        let elements: Vec<String> = (0..5000)
            .map(|index| base64_encode(format!("element-{index}").as_bytes()))
            .collect();
        add_hll_elements(&state, &opened.connection_id, &key, &elements)
            .await
            .map_err(|err| format!("bulk PFADD should succeed: {err:?}"))?;

        let meta = get_key_meta(&state, &opened.connection_id, &key)
            .await
            .map_err(|err| format!("meta should reload: {err:?}"))?;
        match meta.hll.as_ref().map(|hll| hll.encoding.as_str()) {
            Some("dense") => {}
            other => return Err(format!("5,004 elements should force dense, got {other:?}")),
        }

        let count = get_hll_count(&state, &opened.connection_id, &key)
            .await
            .map_err(|err| format!("PFCOUNT should succeed on dense: {err:?}"))?;
        if !(4800..=5200).contains(&count) {
            return Err(format!(
                "5,004 distinct elements should count ≈5,004, got {count}"
            ));
        }

        // dense 的结构：定长 12,304 B；非零寄存器数应落在生日碰撞的期望附近
        // （16384 × (1 − e^(−5004/16384)) ≈ 4,312）—— 这一条同时钉住 6-bit 解包
        let dense = inspect_hll(&state, &opened.connection_id, &key)
            .await
            .map_err(|err| format!("dense inspect should succeed: {err:?}"))?;
        let header = dense.header.ok_or("the dense header should parse")?;
        if header.encoding != "dense" {
            return Err(format!("expected dense, got {}", header.encoding));
        }
        if dense.actual_bytes != 12304 || dense.expected_bytes != Some(12304) {
            return Err(format!(
                "dense should be exactly 12,304 B, got actual={} expected={:?}",
                dense.actual_bytes, dense.expected_bytes
            ));
        }
        if dense.valid_registers != 16384 {
            return Err(format!(
                "a whole dense HLL should have every register valid, got {}",
                dense.valid_registers
            ));
        }
        let registers = base64_decode(
            dense
                .registers_base64
                .as_deref()
                .ok_or("dense registers should unpack")?,
        );
        let nonzero = registers.iter().filter(|value| **value > 0).count();
        if !(4000..=4700).contains(&nonzero) {
            return Err(format!(
                "expected ≈4,312 nonzero registers for 5,004 elements, got {nonzero}"
            ));
        }

        Ok::<_, String>(())
    }
    .await;

    let cleanup = delete_keys(&state, &opened.connection_id, &[key]).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");
    cleanup.expect("test key should be removed");
    result.expect("the HLL path should detect, count and inspect against a real server");
}

/// 损坏值的诚实降级：截断的 dense 被服务端拒绝（WRONGTYPE）但头部与前缀寄存器
/// 仍可解析；sparse 操作码坏了不给寄存器；伪魔数根本不识别成 HLL。
/// 这三档对应 HllViewer 的回退态分层，见 doc/backend/hll.md「inspect：降级逐字段」。
#[tokio::test]
#[serial]
#[ignore = "requires a local Redis instance"]
async fn corrupted_hyperloglogs_degrade_honestly() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let truncated_key = format!("values:hll:truncated:{token}");
    let bad_sparse_key = format!("values:hll:badsparse:{token}");
    let fake_key = format!("values:hll:fake:{token}");

    // 基数缓存必须标成失效（最高 bit = 1）：缓存有效时 PFCOUNT 直接返回缓存值、
    // 根本不走寄存器 —— 一个坏 sparse 会安静地回 0 而不是报错。
    fn hll_header(encoding: u8) -> Vec<u8> {
        let mut bytes = b"HYLL".to_vec();
        bytes.push(encoding);
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(&(1u64 << 63).to_le_bytes());
        bytes
    }

    let result = async {
        // 截断的 dense：合法头部 + 6,000 B 寄存器（应为 12,288）
        let mut truncated = hll_header(0);
        truncated.extend_from_slice(&[0b0010_0110u8; 6000]);
        set_string_value(
            &state,
            &opened.connection_id,
            &truncated_key,
            &base64_encode(&truncated),
        )
        .await
        .map_err(|err| format!("seeding the truncated dense should succeed: {err:?}"))?;

        // 识别只看头部，这是刻意的：要先进 HLL 查看器才轮得到它降级
        let meta = get_key_meta(&state, &opened.connection_id, &truncated_key)
            .await
            .map_err(|err| format!("meta should load: {err:?}"))?;
        match meta.hll.as_ref().map(|hll| hll.encoding.as_str()) {
            Some("dense") => {}
            other => {
                return Err(format!(
                    "the valid header should be detected, got {other:?}"
                ))
            }
        }

        let err = get_hll_count(&state, &opened.connection_id, &truncated_key)
            .await
            .err()
            .ok_or("PFCOUNT should refuse a truncated dense HLL")?;
        let text = format!("{err:?}");
        if !text.contains("WRONGTYPE") {
            return Err(format!("the refusal should be a WRONGTYPE, got {text}"));
        }

        let inspect = inspect_hll(&state, &opened.connection_id, &truncated_key)
            .await
            .map_err(|err| format!("inspect should still work on a truncated value: {err:?}"))?;
        if inspect.header.as_ref().map(|h| h.encoding.as_str()) != Some("dense") {
            return Err("the truncated header should still parse".into());
        }
        // 6,000 B × 8 / 6 = 8,000 个完整寄存器
        if inspect.valid_registers != 8000 {
            return Err(format!(
                "6,000 register bytes should yield 8,000 valid registers, got {}",
                inspect.valid_registers
            ));
        }
        if inspect.actual_bytes != 6016 || inspect.expected_bytes != Some(12304) {
            return Err(format!(
                "the length mismatch should be reported, got actual={} expected={:?}",
                inspect.actual_bytes, inspect.expected_bytes
            ));
        }
        let registers = base64_decode(
            inspect
                .registers_base64
                .as_deref()
                .ok_or("the truncated prefix should still unpack")?,
        );
        if registers[0] == 0 || registers[8000..].iter().any(|value| *value != 0) {
            return Err("the prefix should unpack and the missing tail should be zero".into());
        }

        // sparse 操作码损坏：ZERO run 远不够 16,384 → 不给寄存器，只降级到头部
        let mut bad_sparse = hll_header(1);
        bad_sparse.push(0b0000_0011);
        set_string_value(
            &state,
            &opened.connection_id,
            &bad_sparse_key,
            &base64_encode(&bad_sparse),
        )
        .await
        .map_err(|err| format!("seeding the bad sparse should succeed: {err:?}"))?;

        let err = get_hll_count(&state, &opened.connection_id, &bad_sparse_key)
            .await
            .err()
            .ok_or("PFCOUNT should refuse a corrupt sparse HLL")?;
        let text = format!("{err:?}");
        // 头部合法所以不是 WRONGTYPE，而是计数中途的 INVALIDOBJ ——
        // 前端回退态的判据要两种措辞都认
        if !text.contains("WRONGTYPE") && !text.contains("INVALIDOBJ") {
            return Err(format!(
                "the refusal should name the corruption, got {text}"
            ));
        }

        let inspect = inspect_hll(&state, &opened.connection_id, &bad_sparse_key)
            .await
            .map_err(|err| format!("inspect should still work on a bad sparse: {err:?}"))?;
        if inspect.header.as_ref().map(|h| h.encoding.as_str()) != Some("sparse") {
            return Err("the bad sparse header should still parse".into());
        }
        if inspect.registers_base64.is_some() || inspect.valid_registers != 0 {
            return Err("a corrupt sparse must not pretend to have registers".into());
        }

        // 伪魔数：以 HYLL 开头的普通字符串不该被识别成 HLL
        set_string_value(
            &state,
            &opened.connection_id,
            &fake_key,
            &base64_encode(b"HYLLO WORLD, THIS IS TEXT LONGER THAN 16 BYTES"),
        )
        .await
        .map_err(|err| format!("seeding the fake should succeed: {err:?}"))?;
        let meta = get_key_meta(&state, &opened.connection_id, &fake_key)
            .await
            .map_err(|err| format!("meta should load: {err:?}"))?;
        if meta.hll.is_some() {
            return Err("a fake HYLL prefix must not be detected as an HLL".into());
        }

        Ok::<_, String>(())
    }
    .await;

    let cleanup = delete_keys(
        &state,
        &opened.connection_id,
        &[truncated_key, bad_sparse_key, fake_key],
    )
    .await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");
    cleanup.expect("test keys should be removed");
    result.expect("corrupted HLLs should degrade honestly");
}
