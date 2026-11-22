//! v0.4 批量删除的端到端测试。
//!
//! 这里的每一条断言都是「真实服务端到底怎么匹配」，单元测试一条都碰不到 ——
//! 单元测试喂的是手工构造的 `redis::Value`，而 `MATCH` 的语义在服务端的
//! `stringmatchlen` 里，本地怎么构造都问不出来：
//!
//! - key 名里的 `[` 会把前缀拼出来的模式变成字符类。`evil[ab]:*` 匹配不到
//!   `evil[ab]:1` 本身，却会命中 `evila:1` —— 一次删掉无关 key 的删除。
//!   前端 `src/lib/keyTree.ts` 的 `escapeGlob` 就是为这个存在的，
//!   而它到底管不管用，只有真服务端说了算。
//! - 模式是否被偷偷补了 `*`：`cache:page:home` 与 `cache:page:home*` 在
//!   本地看不出差别，在服务端上差一个 `home:v2`。
//! - 试算有没有真的不碰服务端。
//! - 集群下按节点发的 pipeline 有没有发对节点。发错了**结果照样正确**——redis-rs 会跟着
//!   MOVED 把每条命令转发过去——只是每条多一跳。所以「删完之后不存在了」证明不了路由，
//!   要看服务端各节点的 `rejected_calls`（`rejected_del_calls`）。
//!
//! 任务的启动入口要 `AppHandle`，这里没有，所以直接驱动它下面那层
//! （`delete_chunk_for_connection` / `delete_key_chunk` / `scan_one_page*`）。

use serial_test::serial;
use tsubasa_redis_manager_lib::models::{ConnectionConfig, RedisValue};
use tsubasa_redis_manager_lib::services::connection::{
    close_connection, execute_cli, open_connection,
};
use tsubasa_redis_manager_lib::services::task::{
    delete_chunk_for_connection, delete_key_chunk, scan_one_page, scan_one_page_with_nodes,
};
use tsubasa_redis_manager_lib::state::AppState;

const LOCAL_VDS_ENV_HELP: &str = "Copy scripts/ci/local-vds.env.example.ps1 to scripts/ci/local-vds.env.ps1 for ignored VDS E2E tests.";

fn optional_env_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn redis_host() -> String {
    optional_env_string("PHASE5B_REDIS_HOST")
        .or_else(|| optional_env_string("PHASE2_HOST"))
        .unwrap_or_else(|| {
            panic!(
                "PHASE5B_REDIS_HOST or PHASE2_HOST must be set to run ignored VDS E2E tests. {LOCAL_VDS_ENV_HELP}"
            );
        })
}

fn env_port(key: &str, fallback: &str, default: u16) -> u16 {
    std::env::var(key)
        .or_else(|_| std::env::var(fallback))
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn config() -> ConnectionConfig {
    ConnectionConfig {
        host: redis_host(),
        port: env_port("PHASE5B_REDIS_PORT", "PHASE2_STANDALONE_PORT", 16379),
        auth: None,
        username: None,
        // 每个 e2e 文件占一个 db，免得别处留下的 key 拖慢这里的 SCAN
        db: Some(11),
        name: Some("phase5b".into()),
        separator: Some(":".into()),
        connection_read_only: Some(false),
        cluster: None,
        ssl_options: None,
        ssh_options: None,
        sentinel_options: None,
    }
}

fn cluster_config() -> ConnectionConfig {
    let mut config = config();
    config.host = optional_env_string("PHASE5B_CLUSTER_HOST")
        .or_else(|| optional_env_string("PHASE2_CLUSTER_HOST"))
        .unwrap_or_else(redis_host);
    config.port = env_port("PHASE5B_CLUSTER_PORT", "PHASE2_CLUSTER_PORT", 17000);
    // 集群没有 db 的概念
    config.db = None;
    config.cluster = Some(true);
    config.name = Some("phase5b-cluster".into());
    config
}

async fn seed(state: &AppState, connection_id: &str, key: &str) -> Result<(), String> {
    execute_cli(state, connection_id, &format!("SET {key} value"))
        .await
        .map(|_| ())
        .map_err(|err| format!("SET {key} failed: {err}"))
}

async fn exists(state: &AppState, connection_id: &str, key: &str) -> Result<i64, String> {
    let result = execute_cli(state, connection_id, &format!("EXISTS {key}"))
        .await
        .map_err(|err| format!("EXISTS {key} failed: {err}"))?;
    match result.value {
        RedisValue::Integer(count) => Ok(count),
        other => Err(format!(
            "EXISTS {key} returned an unexpected shape: {other:?}"
        )),
    }
}

async fn cleanup(state: &AppState, connection_id: &str, keys: &[String]) {
    for key in keys {
        let _ = execute_cli(state, connection_id, &format!("DEL {key}")).await;
    }
}

/// 把一个模式扫完，收全部命中。
///
/// 一页扫不完就断言，等于把断言建在 `COUNT` 的运气上 —— `COUNT` 是"翻几个桶"
/// 的提示，不是"返回几个 key"。批量删除任务本身也是扫到游标回 0 才停。
/// SCAN 允许重复，所以这里排序去重后再比。
async fn scan_all(
    state: &AppState,
    connection_id: &str,
    pattern: &str,
) -> Result<Vec<String>, String> {
    let runtime = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(connection_id)
            .map(|session| session.runtime.clone())
            .ok_or_else(|| format!("connection not found: {connection_id}"))?
    };
    let mut executor = runtime.executor.lock().await;
    let mut cursor = "0".to_string();
    let mut found = Vec::new();
    for _ in 0..500 {
        let (next_cursor, keys) = scan_one_page(&mut executor, &cursor, Some(pattern), 500)
            .await
            .map_err(|err| format!("SCAN MATCH {pattern} failed: {err}"))?;
        found.extend(keys);
        cursor = next_cursor;
        if cursor == "0" {
            break;
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// 直接删一批 key。单机路径，没有落点可言 —— 传 `None`，
/// `delete_key_chunk` 走一条多 key `DEL`。
async fn delete_directly(
    state: &AppState,
    connection_id: &str,
    keys: &[String],
) -> Result<u64, String> {
    let runtime = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(connection_id)
            .map(|session| session.runtime.clone())
            .ok_or_else(|| format!("connection not found: {connection_id}"))?
    };
    let mut executor = runtime.executor.lock().await;
    delete_key_chunk(&mut executor, keys, None)
        .await
        .map_err(|err| format!("delete_key_chunk failed: {err}"))
}

/// glob 元字符必须转义 —— 这是整个批量删除里最容易删错东西的一条。
///
/// 树里的 `evil[ab]` 段拼出的模式如果不转义，`[ab]` 就成了字符类：
/// **匹配不到 `evil[ab]:1` 本身，却会命中 `evila:1`**。右键那个 namespace
/// 点删除，删掉的是隔壁那个无关的 key，而被点中的那个还在。
///
/// 两个模式都断言，这条才是证明而不是同义反复：转义过的只命中自己，
/// 没转义的**恰好相反**。前端那一半在 `src/lib/keyTree.ts` 的 `escapeGlob`。
#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn scan_only_matches_the_literal_prefix_when_glob_metacharacters_are_escaped() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    // 树上的 namespace 段就是 `evil[ab]`，`matchPatternFor` 从它拼模式
    let victim = format!("phase5b:esc:{token}:evil[ab]:1");
    let bystander = format!("phase5b:esc:{token}:evila:1");
    let targets = vec![victim.clone(), bystander.clone()];

    let result = async {
        for key in &targets {
            seed(&state, &opened.connection_id, key).await?;
        }

        // `matchPatternFor("phase5b:esc:{token}:evil[ab]", ":")` 产出的形状
        let escaped = format!("phase5b:esc:{token}:evil\\[ab\\]:*");
        let raw = format!("phase5b:esc:{token}:evil[ab]:*");
        let escaped_hits = scan_all(&state, &opened.connection_id, &escaped).await?;
        let raw_hits = scan_all(&state, &opened.connection_id, &raw).await?;

        // 删的只有转义那一批 —— 没转义那一批要是也删了，旁观者就没了
        let deleted = delete_directly(&state, &opened.connection_id, &escaped_hits).await?;
        let victim_left = exists(&state, &opened.connection_id, &victim).await?;
        let bystander_left = exists(&state, &opened.connection_id, &bystander).await?;

        Ok::<_, String>((escaped_hits, raw_hits, deleted, victim_left, bystander_left))
    }
    .await;

    cleanup(&state, &opened.connection_id, &targets).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    let (escaped_hits, raw_hits, deleted, victim_left, bystander_left) =
        result.expect("the escape fixture should run");

    assert_eq!(
        escaped_hits,
        vec![victim.clone()],
        "the escaped pattern must match the literal `evil[ab]` prefix and nothing else"
    );
    // 反过来的这一条才是理由：不转义的话服务端把 `[ab]` 读成字符类
    assert_eq!(
        raw_hits,
        vec![bystander.clone()],
        "an unescaped `[ab]` is a character class on the server, so it hits the wrong key"
    );
    assert!(
        !raw_hits.contains(&victim),
        "an unescaped `[ab]` cannot match the key it was built from"
    );

    assert_eq!(deleted, 1);
    assert_eq!(victim_left, 0, "the key under the namespace should be gone");
    assert_eq!(
        bystander_left, 1,
        "escaping is what keeps the unrelated key alive"
    );
}

/// 模式**原样使用**，后端不补 `*`。
///
/// v0.3 之前批量删除有自己的一份 `format!("{pattern}*")`。用户输
/// `...:home` 想精确删掉一个 key，实际执行的是 `...:home*`，
/// `home:v2` 与 `homepage` 一起没了。**界面上写的模式必须就是真正执行的模式**，
/// 理由见 `services/task.rs` 的 `batch_scan_pattern`。这一条只有在服务端上
/// 才有意义 —— 差别全在 `MATCH` 的实参里，本地怎么断言都是在断言自己。
#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn a_pattern_without_a_wildcard_deletes_exactly_one_key() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let home = format!("phase5b:verbatim:{token}:home");
    let nested = format!("phase5b:verbatim:{token}:home:v2");
    let sibling = format!("phase5b:verbatim:{token}:homepage");
    let targets = vec![home.clone(), nested.clone(), sibling.clone()];

    let result = async {
        for key in &targets {
            seed(&state, &opened.connection_id, key).await?;
        }

        // 不带通配符：补一个 `*` 的话，这一页会有三个 key
        let hits = scan_all(&state, &opened.connection_id, &home).await?;
        let deleted = delete_directly(&state, &opened.connection_id, &hits).await?;

        let mut left = Vec::new();
        for key in &targets {
            left.push(exists(&state, &opened.connection_id, key).await?);
        }
        Ok::<_, String>((hits, deleted, left))
    }
    .await;

    cleanup(&state, &opened.connection_id, &targets).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    let (hits, deleted, left) = result.expect("the verbatim-pattern fixture should run");
    assert_eq!(
        hits,
        vec![home.clone()],
        "the pattern must be used as typed"
    );
    assert_eq!(deleted, 1);
    assert_eq!(left, vec![0, 1, 1], "only {home} should have been deleted");
}

/// 试算只量、不碰服务端。
///
/// 试算的全部价值在于"试算出来的数字就是真会改的数字"（`doc/backend/tasks.md`
/// 「试算」），而它敢被点是因为它什么都不删。这条把两半都钉住：`dry_run` 之后
/// 每个 key 都还在，同一批 key 在 `dry_run = false` 下真的被删掉。
#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn dry_run_counts_without_deleting_anything() {
    let state = AppState::default();
    let opened = open_connection(&state, config())
        .await
        .expect("connection should open");
    let token = uuid::Uuid::new_v4();
    let keys = (0..4)
        .map(|index| format!("phase5b:dry:{token}:{index}"))
        .collect::<Vec<_>>();

    let result = async {
        for key in &keys {
            seed(&state, &opened.connection_id, key).await?;
        }

        let dry = delete_chunk_for_connection(&state, &opened.connection_id, &keys, None, true)
            .await
            .map_err(|err| format!("dry run failed: {err}"))?;
        let mut after_dry_run = Vec::new();
        for key in &keys {
            after_dry_run.push(exists(&state, &opened.connection_id, key).await?);
        }

        let real = delete_chunk_for_connection(&state, &opened.connection_id, &keys, None, false)
            .await
            .map_err(|err| format!("real run failed: {err}"))?;
        let mut after_real_run = Vec::new();
        for key in &keys {
            after_real_run.push(exists(&state, &opened.connection_id, key).await?);
        }

        Ok::<_, String>((dry, after_dry_run, real, after_real_run))
    }
    .await;

    cleanup(&state, &opened.connection_id, &keys).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("connection should close");

    let (dry, after_dry_run, real, after_real_run) =
        result.expect("the dry-run fixture should run");
    // 删除数是调用方加进 `succeeded` 的那一格；试算这一格必须是 0，
    // 命中数走 `processed`（chunk 长度），与批量 TTL 同一口径。
    assert_eq!(dry, 0, "a dry run must not claim any deletion");
    assert_eq!(
        after_dry_run,
        vec![1, 1, 1, 1],
        "a dry run must leave every key in place"
    );
    assert_eq!(real, 4);
    assert_eq!(after_real_run, vec![0, 0, 0, 0]);
}

/// 集群下按模式删除，**按节点整批 pipeline** —— 模式路径真正走的那条路。
///
/// 路由错了结果照样正确（redis-rs 跟随 MOVED，见下一条用例），所以计数与「删完不在了」
/// 都证明不了路由。路由的断言是删除前后各主节点 `DEL` 的 `rejected_calls` 没有增加。
///
/// 这里把 `batch.node` 一路传进 `delete_chunk_for_connection`，与
/// `run_batch_delete_task` 的模式路径同形。传 `None` 就退化成逐 key 串行，
/// 那条路由不会错，也就测不到这个改动 —— 这条断言只在带落点时才有意义。
/// 顺带钉住另一件事：同一个节点的一批 key 仍然散在多个槽位上，所以发出去的
/// 必须是「pipeline of 单 key `DEL`」；合成一条多 key `DEL` 会被拒成
/// `CROSSSLOT`，这条会红。
#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn cluster_pattern_delete_removes_every_key_it_scanned() {
    let state = AppState::default();
    let opened = open_connection(&state, cluster_config())
        .await
        .expect("cluster connection should open");
    let token = uuid::Uuid::new_v4();
    // 30 个 key 足以散布到三个主节点上
    let keys = (0..30)
        .map(|index| format!("phase5b:cluster:{token}:{index}"))
        .collect::<Vec<_>>();

    let result = async {
        for key in &keys {
            seed(&state, &opened.connection_id, key).await?;
        }

        let pattern = format!("phase5b:cluster:{token}:*");
        let rejected_before = rejected_del_calls(&state, &opened.connection_id).await?;
        let mut cursor = "0".to_string();
        let mut deleted = 0_u64;
        let mut scanned = 0_usize;
        let mut batch_count = 0_usize;
        // 边扫边删 —— 批量删除任务就是这个形状。SCAN 只保证"全程都在的 key
        // 至少返回一次"，被删掉的那些是我们已经收下的，剩下的不受影响。
        for _ in 0..200 {
            let page = {
                let runtime = {
                    let sessions = state.sessions.lock().await;
                    sessions
                        .get(&opened.connection_id)
                        .map(|session| session.runtime.clone())
                        .ok_or_else(|| "connection not found".to_string())?
                };
                let mut executor = runtime.executor.lock().await;
                scan_one_page_with_nodes(&mut executor, &cursor, Some(&pattern), 10)
                    .await
                    .map_err(|err| format!("cluster scan failed: {err}"))?
            };

            for batch in &page.batches {
                // 落点丢了就没有东西可以按节点分组，整条路径退回逐 key
                let (host, port) = batch
                    .node
                    .as_ref()
                    .ok_or_else(|| "a cluster scan batch must carry its node".to_string())?;
                batch_count += 1;
                scanned += batch.keys.len();
                deleted += delete_chunk_for_connection(
                    &state,
                    &opened.connection_id,
                    &batch.keys,
                    Some((host.as_str(), *port)),
                    false,
                )
                .await
                .map_err(|err| format!("cluster delete failed: {err}"))?;
            }

            cursor = page.cursor;
            if cursor == "0" {
                break;
            }
        }

        let rejected = rejected_del_calls(&state, &opened.connection_id).await? - rejected_before;

        let mut left = Vec::new();
        for key in &keys {
            left.push(exists(&state, &opened.connection_id, key).await?);
        }
        Ok::<_, String>((batch_count, scanned, deleted, rejected, left))
    }
    .await;

    cleanup(&state, &opened.connection_id, &keys).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("cluster connection should close");

    let (batch_count, scanned, deleted, rejected, left) =
        result.expect("the cluster pattern delete should run");
    assert!(
        batch_count > 1,
        "the fixture should spread across more than one master, got {batch_count} batch(es)"
    );
    assert_eq!(scanned, 30, "the scan should have found every seeded key");
    assert_eq!(deleted, 30);
    for (key, left) in keys.iter().zip(left) {
        assert_eq!(left, 0, "{key} survived the delete");
    }
    // 这一条才是路由的断言。上面几条在路由全错时也会通过：redis-rs 会跟着 MOVED
    // 把每条 DEL 转发到持有槽位的节点（见下一条用例）。
    assert_eq!(
        rejected, 0,
        "{rejected} DEL(s) were sent to a node that does not own the slot and got MOVED"
    );
}

/// 打错节点的 pipeline 会被 redis-rs 透明重定向，而不是静默落空。
///
/// 钉住的是依赖库的行为，它决定了 `PipelineTarget` 错了是什么性质的问题：每条打错节点的
/// 命令先在那个节点被 MOVED 拒一次，redis-rs 再把它转发给持有槽位的节点，结果正确，
/// 代价是每条命令多一跳往返——按节点整批发 pipeline 的收益全部抵消。
/// 哪天 redis-rs 不再跟随重定向，这条会红，那时路由错就成了正确性问题。
#[tokio::test]
#[serial]
#[ignore = "requires VDS phase2 compose"]
async fn cluster_pipeline_sent_to_the_wrong_node_is_redirected_not_dropped() {
    let state = AppState::default();
    let opened = open_connection(&state, cluster_config())
        .await
        .expect("cluster connection should open");
    let token = uuid::Uuid::new_v4();
    let keys = (0..30)
        .map(|index| format!("phase5b:redirect:{token}:{index}"))
        .collect::<Vec<_>>();

    let result = async {
        for key in &keys {
            seed(&state, &opened.connection_id, key).await?;
        }

        let pattern = format!("phase5b:redirect:{token}:*");
        let mut batches: Vec<((String, u16), Vec<String>)> = Vec::new();
        let mut cursor = "0".to_string();
        for _ in 0..200 {
            let runtime = {
                let sessions = state.sessions.lock().await;
                sessions
                    .get(&opened.connection_id)
                    .map(|session| session.runtime.clone())
                    .ok_or_else(|| "connection not found".to_string())?
            };
            let mut executor = runtime.executor.lock().await;
            let page = scan_one_page_with_nodes(&mut executor, &cursor, Some(&pattern), 100)
                .await
                .map_err(|err| format!("cluster scan failed: {err}"))?;
            for batch in page.batches {
                let node = batch
                    .node
                    .ok_or_else(|| "a cluster scan batch must carry its node".to_string())?;
                batches.push((node, batch.keys));
            }
            cursor = page.cursor;
            if cursor == "0" {
                break;
            }
        }

        // 一个节点扫出来的那批 key，交给另一个节点去删
        let (home, victims) = batches
            .iter()
            .find(|(_, keys)| !keys.is_empty())
            .cloned()
            .ok_or_else(|| "the scan found nothing".to_string())?;
        let (wrong, _) = batches
            .iter()
            .find(|(node, _)| *node != home)
            .cloned()
            .ok_or_else(|| "the fixture should spread across more than one master".to_string())?;

        let rejected_before = rejected_del_calls(&state, &opened.connection_id).await?;
        let deleted = delete_chunk_for_connection(
            &state,
            &opened.connection_id,
            &victims,
            Some((wrong.0.as_str(), wrong.1)),
            false,
        )
        .await
        .map_err(|err| format!("delete sent to the wrong node failed: {err}"))?;
        let rejected = rejected_del_calls(&state, &opened.connection_id).await? - rejected_before;

        let mut left = 0;
        for key in &victims {
            left += exists(&state, &opened.connection_id, key).await?;
        }
        Ok::<_, String>((victims.len() as u64, deleted, rejected, left))
    }
    .await;

    cleanup(&state, &opened.connection_id, &keys).await;
    close_connection(&state, &opened.connection_id)
        .await
        .expect("cluster connection should close");

    let (sent, deleted, rejected, left) = result.expect("the misrouted delete should run");
    assert_eq!(
        rejected, sent,
        "every DEL should have hit the wrong node first and got MOVED"
    );
    assert_eq!(
        deleted, sent,
        "redis-rs should have followed MOVED to the slot owner"
    );
    assert_eq!(left, 0);
}

/// 集群各主节点上 `DEL` 被 MOVED 拒掉的累计次数（`INFO commandstats` 的 `rejected_calls`）。
///
/// 路由对不对，只能从服务端看：打错节点的命令会被 redis-rs 转发到正确的节点，结果照样正确，
/// 只在被打错的那个节点上留下一次 `rejected_calls`。`rejected_calls` 是 Redis 7.0 加的字段。
async fn rejected_del_calls(state: &AppState, connection_id: &str) -> Result<u64, String> {
    let result = execute_cli(state, connection_id, "CLUSTER NODES")
        .await
        .map_err(|err| format!("CLUSTER NODES failed: {err}"))?;
    let RedisValue::String(nodes) = result.value else {
        return Err(format!(
            "CLUSTER NODES returned an unexpected shape: {:?}",
            result.value
        ));
    };

    let mut total = 0;
    for line in nodes.lines() {
        // <id> <ip:port@cport[,hostname]> <flags> …
        let mut fields = line.split_whitespace();
        let (Some(_), Some(address), Some(flags)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if !flags.split(',').any(|flag| flag == "master") {
            continue;
        }
        let address = address.split('@').next().unwrap_or(address);
        let client = redis::Client::open(format!("redis://{address}/"))
            .map_err(|err| format!("bad node address {address}: {err}"))?;
        let mut connection = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|err| format!("connect to {address} failed: {err}"))?;
        let info: String = redis::cmd("INFO")
            .arg("commandstats")
            .query_async(&mut connection)
            .await
            .map_err(|err| format!("INFO commandstats on {address} failed: {err}"))?;
        total += rejected_calls(&info, "cmdstat_del").map_err(|err| format!("{address}: {err}"))?;
    }
    Ok(total)
}

/// 一个节点从没执行过这条命令时没有那一行，计 0；有那一行却没有 `rejected_calls`，
/// 说明服务端早于 Redis 7.0，这时数不出来，必须报错而不是当成 0。
fn rejected_calls(info: &str, stat: &str) -> Result<u64, String> {
    let Some(fields) = info
        .lines()
        .find_map(|line| line.trim().strip_prefix(stat)?.strip_prefix(':'))
    else {
        return Ok(0);
    };
    fields
        .split(',')
        .find_map(|field| field.strip_prefix("rejected_calls="))
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| "commandstats has no rejected_calls (needs Redis 7.0+)".to_string())
}
