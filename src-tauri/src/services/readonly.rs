//! 只读连接的写命令拦截 —— **只服务 CLI**。
//!
//! 这里是按命令名匹配的，"不在列表里就算写"。这种猜法只在一个场景下站得住：
//! CLI 里用户手输的是自由文本，除了看命令名没有别的判据。
//!
//! 应用自己的功能路径**不走这里**。它们的读写性质在编译期就确定了：
//! 取值命令天生只读，写入命令在 `connection::writable_runtime` 显式判会话状态。
//! v0.1 时两种流量都挤在 `redis_execute_command` 上，于是「内存列想显示数据」
//! 这样一个功能需求，逼得安全边界要为 `MEMORY` 开一个洞 —— 而 `MEMORY PURGE`
//! 和 `CONFIG SET` 会跟着一起放行。分层归位之后这个矛盾就不存在了，
//! 所以允许列表里**不加** MEMORY 与 CONFIG。
//! 见 doc/backend/sessions.md「只读属于会话」与「不做」。

const READ_COMMANDS: &[&str] = &[
    "BITCOUNT",
    "BITPOS",
    "DBSIZE",
    "DUMP",
    "ECHO",
    "EXISTS",
    "GET",
    "GETBIT",
    "GETRANGE",
    "GEOHASH",
    "GEOPOS",
    "GEODIST",
    "HEXISTS",
    "HGET",
    "HGETALL",
    "HKEYS",
    "HLEN",
    "HMGET",
    "HSCAN",
    "HSTRLEN",
    "HVALS",
    "INFO",
    "KEYS",
    "LASTSAVE",
    "LINDEX",
    "LLEN",
    "LRANGE",
    "MGET",
    "OBJECT",
    // PFCOUNT 技术上会回写基数缓存（8 字节，逻辑值不变），但 ACL 归类是 @read、
    // 只读副本可执行 —— 只读会话保护的是数据不是服务器的每个内部字节。
    // PFADD / PFMERGE 是真正的写，不进这张表。
    "PFCOUNT",
    "PING",
    "PTTL",
    "RANDOMKEY",
    "ROLE",
    "SCAN",
    "SCARD",
    "SDIFF",
    "SINTER",
    "SISMEMBER",
    "SMEMBERS",
    "SRANDMEMBER",
    "SSCAN",
    "STRLEN",
    "SUNION",
    "TIME",
    "TTL",
    "TYPE",
    "XINFO",
    "XLEN",
    "XPENDING",
    "XRANGE",
    "XREVRANGE",
    "XREAD",
    "ZCARD",
    "ZCOUNT",
    "ZLEXCOUNT",
    "ZRANGE",
    "ZRANGEBYLEX",
    "ZRANGEBYSCORE",
    "ZRANK",
    "ZREVRANGE",
    "ZREVRANGEBYLEX",
    "ZREVRANGEBYSCORE",
    "ZREVRANK",
    "ZSCAN",
    "ZSCORE",
];

pub fn is_write_command(command: &str) -> bool {
    let upper = command.to_ascii_uppercase();
    !READ_COMMANDS.contains(&upper.as_str())
}

#[cfg(test)]
mod tests {
    use super::is_write_command;

    #[test]
    fn detects_write_command_case_insensitively() {
        assert!(is_write_command("set"));
        assert!(is_write_command("HSET"));
    }

    #[test]
    fn detects_scripting_geo_stream_and_admin_write_commands() {
        assert!(is_write_command("eval"));
        assert!(is_write_command("GEOADD"));
        assert!(is_write_command("xtrim"));
        assert!(is_write_command("SCRIPT"));
        assert!(is_write_command("FUNCTION"));
        assert!(is_write_command("CONFIG"));
    }

    #[test]
    fn detects_consumer_group_stream_write_commands() {
        assert!(is_write_command("XCLAIM"));
        assert!(is_write_command("xautoclaim"));
        assert!(is_write_command("XREADGROUP"));
    }

    #[test]
    fn conservatively_blocks_side_effecting_command_families() {
        for command in [
            "BITFIELD",
            "GEORADIUS",
            "GEORADIUSBYMEMBER",
            "ACL",
            "MODULE",
            "CLIENT",
            "CLUSTER",
            "REPLICAOF",
            "SLAVEOF",
            "SAVE",
            "BGSAVE",
            "BGREWRITEAOF",
            "SHUTDOWN",
            "LATENCY",
            "SLOWLOG",
            "MEMORY",
        ] {
            assert!(is_write_command(command), "{command} should be blocked");
        }
    }

    #[test]
    fn blocks_unknown_module_and_future_commands() {
        for command in [
            "JSON.SET",
            "FT.CREATE",
            "TS.ADD",
            "GETSET",
            "PUBLISH",
            "ZRANGESTORE",
            "XREADGROUP",
        ] {
            assert!(is_write_command(command), "{command} should be blocked");
        }
    }

    #[test]
    fn allows_read_command() {
        assert!(!is_write_command("get"));
        assert!(!is_write_command("scan"));
        assert!(!is_write_command("PING"));
    }
}
