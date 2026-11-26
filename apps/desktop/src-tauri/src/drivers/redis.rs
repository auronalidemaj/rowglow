//! Redis (and Valkey / KeyDB / Dragonfly) browser: scan keys, read and edit
//! every core type, and a console for raw commands.
//!
//! Safety:
//! * Read-only connections only allow commands on an allow-list of reads.
//! * Destructive commands (FLUSHALL, KEYS, CONFIG SET, …) need an explicit
//!   confirmation from the UI.
//! * Commands that would hijack the shared connection (SELECT, SUBSCRIBE,
//!   MONITOR, blocking pops) are refused in the console.
//! * Renames never overwrite (RENAMENX) and new keys never replace existing ones.
//!
//! Topologies: a single server, Redis Cluster (keys are scanned node by node
//! across the primaries; commands are routed by slot) and Redis Sentinel (the
//! Sentinels name the current primary, or a replica, and Rowglow connects to
//! it; reconnecting asks them again, so a failover is followed).

use crate::error::{AppError, AppResult};
use crate::model::*;
use base64::Engine as _;
use redis::aio::{ConnectionLike, MultiplexedConnection};
use redis::cluster_async::ClusterConnection;
use redis::cluster_routing::{RoutingInfo, SingleNodeRoutingInfo};
use redis::sentinel::{SentinelClientBuilder, SentinelServerType};
use redis::{AsyncConnectionConfig, Cmd, ConnectionAddr, IntoConnectionInfo, Pipeline, RedisFuture, TlsMode, Value};
use serde::Deserialize;
use serde_json::{json, Value as Json};
use std::time::Duration;

#[derive(Clone)]
pub struct RedisSession {
    conn: Conn,
    pub db: i64,
    read_only: bool,
}

/// A single server (also what Sentinel hands out) or a cluster.
#[derive(Clone)]
enum Conn {
    Single(MultiplexedConnection),
    Cluster(ClusterConnection),
}

impl ConnectionLike for Conn {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        match self {
            Conn::Single(c) => c.req_packed_command(cmd),
            Conn::Cluster(c) => c.req_packed_command(cmd),
        }
    }
    fn req_packed_commands<'a>(&'a mut self, pipeline: &'a Pipeline, offset: usize, count: usize) -> RedisFuture<'a, Vec<Value>> {
        match self {
            Conn::Single(c) => c.req_packed_commands(pipeline, offset, count),
            Conn::Cluster(c) => c.req_packed_commands(pipeline, offset, count),
        }
    }
    fn get_db(&self) -> i64 {
        match self {
            Conn::Single(c) => c.get_db(),
            Conn::Cluster(c) => c.get_db(),
        }
    }
}

/// One node of a cluster, from CLUSTER NODES.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterNode {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub primary: bool,
    pub failed: bool,
    pub primary_id: String,
    pub slots: String,
}

/// Parses CLUSTER NODES output.
pub fn parse_cluster_nodes(text: &str) -> Vec<ClusterNode> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 8 {
            continue;
        }
        // ip:port@cport[,hostname]
        let addr = f[1].split(',').next().unwrap_or("");
        let addr = addr.split('@').next().unwrap_or("");
        let (host, port) = split_host_port(addr, 6379);
        if host.is_empty() || port == 0 {
            continue;
        }
        let flags = f[2];
        out.push(ClusterNode {
            id: f[0].to_string(),
            host,
            port,
            primary: flags.split(',').any(|x| x == "master"),
            failed: flags.split(',').any(|x| x == "fail" || x == "noaddr" || x == "handshake"),
            primary_id: if f[3] == "-" { String::new() } else { f[3].to_string() },
            slots: f[8..].join(" "),
        });
    }
    out.sort_by(|a, b| (a.host.as_str(), a.port).cmp(&(b.host.as_str(), b.port)));
    out
}

/// Cluster SCAN cursor: which primary we are on, and that node's own cursor.
fn parse_cluster_cursor(c: &str) -> (usize, u64) {
    match c.split_once(':') {
        Some((n, c)) => (n.parse().unwrap_or(0), c.parse().unwrap_or(0)),
        None => (0, 0),
    }
}

const MAX_ITEMS: usize = 1000;
const MAX_STRING: usize = 1024 * 1024;

pub fn redis_error(e: redis::RedisError) -> AppError {
    let msg = e.to_string();
    let kind = if e.is_io_error() || e.is_connection_dropped() || e.is_timeout() || e.is_unrecoverable_error() {
        "connection"
    } else {
        "db"
    };
    let lower = msg.to_lowercase();
    let mut err = AppError::new(kind, msg.clone());
    if let Some(code) = e.code() {
        err = err.with_code(code.to_string());
    }
    if lower.contains("noauth") || lower.contains("wrongpass") || lower.contains("invalid password") || lower.contains("invalid username-password") {
        err.kind = "connection".into();
        err.message = format!("Authentication failed: {msg}");
    }
    err
}

fn cluster_error(e: redis::RedisError) -> AppError {
    let lower = e.to_string().to_lowercase();
    let mut err = redis_error(e);
    if lower.contains("cluster support disabled") || lower.contains("this instance has cluster support disabled") {
        err.message = "This server isn't running in cluster mode. Choose Single server instead.".into();
    } else if err.kind == "connection" && !err.message.starts_with("Authentication") {
        err.message = format!(
            "{} If the cluster runs in Docker or behind NAT, its nodes may announce addresses this computer can't reach (cluster-announce-ip).",
            err.message
        );
    }
    err
}

fn sentinel_error(e: redis::RedisError, master: &str) -> AppError {
    let lower = e.to_string().to_lowercase();
    let mut err = redis_error(e);
    if lower.contains("masternamenotfound")
        || lower.contains("name not found")
        || lower.contains("no master")
        || lower.contains("could not find")
        || lower.contains("master not found")
    {
        err.message = format!("The Sentinels don't know a master named “{master}”. Check the master name (sentinel monitor <name> … in sentinel.conf).");
    }
    err
}

fn key_bytes(key: &str, key_b64: &Option<String>) -> AppResult<Vec<u8>> {
    match key_b64 {
        Some(b) if !b.is_empty() => base64::engine::general_purpose::STANDARD
            .decode(b)
            .map_err(|_| AppError::invalid("Invalid key encoding")),
        _ => Ok(key.as_bytes().to_vec()),
    }
}

/// A key or value for the UI: text when it is UTF-8, base64 otherwise.
fn text_or_b64(bytes: &[u8]) -> Json {
    match std::str::from_utf8(bytes) {
        Ok(s) => json!({ "text": s }),
        Err(_) => json!({ "b64": base64::engine::general_purpose::STANDARD.encode(bytes), "text": String::from_utf8_lossy(bytes) }),
    }
}

fn bytes_of(v: &Value) -> Vec<u8> {
    match v {
        Value::BulkString(b) => b.clone(),
        Value::SimpleString(s) => s.as_bytes().to_vec(),
        Value::VerbatimString { text, .. } => text.as_bytes().to_vec(),
        Value::Int(i) => i.to_string().into_bytes(),
        Value::Double(d) => d.to_string().into_bytes(),
        Value::Okay => b"OK".to_vec(),
        _ => Vec::new(),
    }
}

fn int_of(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        Value::BulkString(b) => std::str::from_utf8(b).ok().and_then(|s| s.parse().ok()).unwrap_or(0),
        Value::SimpleString(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn items_of(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) | Value::Set(a) => a,
        Value::Map(m) => m.into_iter().flat_map(|(k, v)| [k, v]).collect(),
        Value::Attribute { data, .. } => items_of(*data),
        _ => Vec::new(),
    }
}

/// Renders a reply like redis-cli does, as a JSON tree for the console.
pub fn reply_json(v: &Value) -> Json {
    match v {
        Value::Nil => json!({ "t": "nil" }),
        Value::Int(i) => json!({ "t": "int", "v": i }),
        Value::BulkString(b) => {
            let mut j = text_or_b64(b);
            j["t"] = json!("str");
            j
        }
        Value::SimpleString(s) => json!({ "t": "status", "v": s }),
        Value::Okay => json!({ "t": "status", "v": "OK" }),
        Value::Double(d) => json!({ "t": "double", "v": d }),
        Value::Boolean(b) => json!({ "t": "bool", "v": b }),
        Value::VerbatimString { text, .. } => json!({ "t": "str", "text": text }),
        Value::BigNumber(n) => json!({ "t": "bignum", "v": format!("{n:?}") }),
        Value::Array(a) | Value::Set(a) => json!({ "t": "array", "v": a.iter().map(reply_json).collect::<Vec<_>>() }),
        Value::Push { data, .. } => json!({ "t": "array", "v": data.iter().map(reply_json).collect::<Vec<_>>() }),
        Value::Map(m) => json!({ "t": "map", "v": m.iter().map(|(k, v)| json!([reply_json(k), reply_json(v)])).collect::<Vec<_>>() }),
        Value::Attribute { data, .. } => reply_json(data),
        Value::ServerError(e) => json!({ "t": "error", "v": e.to_string() }),
        // Value is non-exhaustive.
        other => json!({ "t": "str", "text": format!("{other:?}") }),
    }
}

// ------------------------------------------------------------ command safety

/// Commands allowed on read-only connections.
const READ_COMMANDS: &[&str] = &[
    "GET", "MGET", "GETRANGE", "STRLEN", "EXISTS", "TYPE", "TTL", "PTTL", "EXPIRETIME", "PEXPIRETIME", "SCAN", "DBSIZE", "RANDOMKEY",
    "HGET", "HMGET", "HGETALL", "HKEYS", "HVALS", "HLEN", "HEXISTS", "HSCAN", "HSTRLEN", "HRANDFIELD", "LRANGE", "LLEN", "LINDEX",
    "LPOS", "SMEMBERS", "SCARD", "SISMEMBER", "SMISMEMBER", "SSCAN", "SRANDMEMBER", "SINTER", "SUNION", "SDIFF", "SINTERCARD",
    "ZRANGE", "ZRANGEBYSCORE", "ZRANGEBYLEX", "ZREVRANGE", "ZREVRANGEBYSCORE", "ZREVRANGEBYLEX", "ZCARD", "ZCOUNT", "ZLEXCOUNT",
    "ZSCORE", "ZMSCORE", "ZRANK", "ZREVRANK", "ZSCAN", "ZRANDMEMBER", "ZINTER", "ZUNION", "ZDIFF", "ZINTERCARD", "XRANGE",
    "XREVRANGE", "XLEN", "XINFO", "XPENDING", "GETBIT", "BITCOUNT", "BITPOS", "BITFIELD_RO", "PFCOUNT", "GEOPOS", "GEODIST",
    "GEOHASH", "GEOSEARCH", "GEORADIUS_RO", "GEORADIUSBYMEMBER_RO", "INFO", "PING", "ECHO", "TIME", "LASTSAVE", "ROLE", "MEMORY",
    "OBJECT", "DUMP", "SORT_RO", "LCS", "COMMAND", "SLOWLOG", "LATENCY", "CLIENT", "CONFIG", "ACL", "JSON.GET", "JSON.MGET",
    "JSON.TYPE", "JSON.STRLEN", "JSON.ARRLEN", "JSON.OBJKEYS", "JSON.OBJLEN", "FT.SEARCH", "FT.INFO", "FT._LIST", "FT.AGGREGATE",
    "TS.GET", "TS.RANGE", "TS.REVRANGE", "TS.INFO", "TS.MGET", "TS.MRANGE", "HELLO", "MODULE", "CLUSTER", "FUNCTION",
];
/// Read commands whose subcommands can still change state.
const READ_SUBCOMMAND_WRITES: &[(&str, &[&str])] = &[
    ("CLIENT", &["KILL", "PAUSE", "UNPAUSE", "NO-EVICT", "NO-TOUCH", "SETNAME", "REPLY", "TRACKING", "UNBLOCK"]),
    ("CONFIG", &["SET", "RESETSTAT", "REWRITE"]),
    ("ACL", &["SETUSER", "DELUSER", "LOAD", "SAVE"]),
    ("MEMORY", &["PURGE"]),
    ("SLOWLOG", &["RESET"]),
    ("LATENCY", &["RESET"]),
    ("MODULE", &["LOAD", "UNLOAD", "LOADEX"]),
    ("CLUSTER", &["ADDSLOTS", "DELSLOTS", "FAILOVER", "FORGET", "MEET", "RESET", "SETSLOT", "REPLICATE", "FLUSHSLOTS", "SAVECONFIG"]),
    ("FUNCTION", &["DELETE", "FLUSH", "LOAD", "RESTORE"]),
];
/// Commands that need an explicit "yes, run it".
const DANGEROUS: &[&str] = &[
    "FLUSHALL", "FLUSHDB", "SHUTDOWN", "DEBUG", "KEYS", "SWAPDB", "MIGRATE", "REPLICAOF", "SLAVEOF", "FAILOVER", "SAVE", "RESTORE",
];
/// Commands that would take over or block the shared connection.
const REFUSED: &[&str] = &[
    "SELECT", "SUBSCRIBE", "PSUBSCRIBE", "SSUBSCRIBE", "UNSUBSCRIBE", "PUNSUBSCRIBE", "MONITOR", "BLPOP", "BRPOP", "BLMOVE",
    "BRPOPLPUSH", "BZPOPMIN", "BZPOPMAX", "BLMPOP", "BZMPOP", "WAIT", "WAITAOF", "QUIT", "RESET", "MULTI", "EXEC", "DISCARD",
    "WATCH", "UNWATCH", "SYNC", "PSYNC", "AUTH",
];

pub fn is_read_command(args: &[String]) -> bool {
    let Some(cmd) = args.first().map(|c| c.to_ascii_uppercase()) else {
        return true;
    };
    if !READ_COMMANDS.contains(&cmd.as_str()) {
        return false;
    }
    let sub = args.get(1).map(|s| s.to_ascii_uppercase()).unwrap_or_default();
    if let Some((_, writes)) = READ_SUBCOMMAND_WRITES.iter().find(|(c, _)| *c == cmd) {
        return !writes.contains(&sub.as_str());
    }
    true
}

pub fn danger_reason(args: &[String]) -> Option<String> {
    let cmd = args.first()?.to_ascii_uppercase();
    let sub = args.get(1).map(|s| s.to_ascii_uppercase()).unwrap_or_default();
    match cmd.as_str() {
        "FLUSHALL" => Some("FLUSHALL deletes every key in every database on this server.".into()),
        "FLUSHDB" => Some("FLUSHDB deletes every key in this database.".into()),
        "KEYS" => Some("KEYS scans the whole keyspace in one go and blocks the server while it runs. The key browser uses SCAN instead.".into()),
        "SHUTDOWN" => Some("SHUTDOWN stops the Redis server.".into()),
        "CONFIG" if sub == "SET" || sub == "REWRITE" => Some("This changes the server's configuration.".into()),
        "SCRIPT" | "FUNCTION" if sub == "FLUSH" => Some("This deletes every loaded script / function.".into()),
        "CLIENT" if sub == "KILL" => Some("This disconnects other clients.".into()),
        "DEL" | "UNLINK" if args.len() > 11 => Some(format!("This deletes {} keys.", args.len() - 1)),
        c if DANGEROUS.contains(&c) => Some(format!("{c} can disrupt the server or lose data.")),
        _ => None,
    }
}

/// Splits a console line into arguments like redis-cli: "double quotes" with
/// \n \t \" \\ \xHH escapes (raw bytes), 'single quotes' taken literally.
pub fn split_args_bytes(line: &str) -> AppResult<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let push_char = |buf: &mut Vec<u8>, c: char| {
        let mut tmp = [0u8; 4];
        buf.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
    };
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let mut cur: Vec<u8> = Vec::new();
        let mut quoted = false;
        while i < chars.len() && !chars[i].is_whitespace() {
            let c = chars[i];
            if c == '"' {
                quoted = true;
                i += 1;
                loop {
                    let Some(&ch) = chars.get(i) else {
                        return Err(AppError::invalid("Unbalanced double quote"));
                    };
                    i += 1;
                    match ch {
                        '"' => break,
                        '\\' => {
                            let Some(&e) = chars.get(i) else {
                                return Err(AppError::invalid("Unfinished escape"));
                            };
                            i += 1;
                            match e {
                                'n' => cur.push(b'\n'),
                                't' => cur.push(b'\t'),
                                'r' => cur.push(b'\r'),
                                'a' => cur.push(7),
                                'b' => cur.push(8),
                                'x' => {
                                    let hex: String = chars.get(i..i + 2).map(|s| s.iter().collect()).unwrap_or_default();
                                    match u8::from_str_radix(&hex, 16) {
                                        Ok(b) => {
                                            cur.push(b);
                                            i += 2;
                                        }
                                        Err(_) => cur.push(b'x'),
                                    }
                                }
                                other => push_char(&mut cur, other),
                            }
                        }
                        other => push_char(&mut cur, other),
                    }
                }
            } else if c == '\'' {
                quoted = true;
                i += 1;
                loop {
                    let Some(&ch) = chars.get(i) else {
                        return Err(AppError::invalid("Unbalanced single quote"));
                    };
                    i += 1;
                    if ch == '\'' {
                        break;
                    }
                    push_char(&mut cur, ch);
                }
            } else {
                push_char(&mut cur, c);
                i += 1;
            }
        }
        if !cur.is_empty() || quoted {
            out.push(cur);
        }
    }
    Ok(out)
}

/// Text form of the arguments, for classifying commands.
pub fn split_args(line: &str) -> AppResult<Vec<String>> {
    Ok(split_args_bytes(line)?.iter().map(|a| String::from_utf8_lossy(a).into_owned()).collect())
}

// --------------------------------------------------------------- operations

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum RedisOp {
    #[serde(rename_all = "camelCase")]
    SetString { key: String, key_b64: Option<String>, value: String },
    #[serde(rename_all = "camelCase")]
    HashSet { key: String, key_b64: Option<String>, field: String, value: String, old_field: Option<String> },
    #[serde(rename_all = "camelCase")]
    HashDel { key: String, key_b64: Option<String>, fields: Vec<String> },
    #[serde(rename_all = "camelCase")]
    ListSet { key: String, key_b64: Option<String>, index: i64, value: String, expected: Option<String> },
    #[serde(rename_all = "camelCase")]
    ListPush { key: String, key_b64: Option<String>, value: String, head: bool },
    #[serde(rename_all = "camelCase")]
    ListRemove { key: String, key_b64: Option<String>, index: i64, expected: String },
    #[serde(rename_all = "camelCase")]
    SetAdd { key: String, key_b64: Option<String>, members: Vec<String> },
    #[serde(rename_all = "camelCase")]
    SetRemove { key: String, key_b64: Option<String>, members: Vec<String> },
    #[serde(rename_all = "camelCase")]
    ZsetAdd { key: String, key_b64: Option<String>, member: String, score: f64 },
    #[serde(rename_all = "camelCase")]
    ZsetRemove { key: String, key_b64: Option<String>, members: Vec<String> },
    #[serde(rename_all = "camelCase")]
    Create { key: String, kind: String, value: String, field: Option<String>, score: Option<f64>, ttl: Option<i64> },
    #[serde(rename_all = "camelCase")]
    Expire { key: String, key_b64: Option<String>, ttl: Option<i64> },
    #[serde(rename_all = "camelCase")]
    Rename { key: String, key_b64: Option<String>, new_key: String },
    #[serde(rename_all = "camelCase")]
    Delete { keys: Vec<String> },
}

fn cmd(name: &str) -> redis::Cmd {
    redis::cmd(name)
}

impl RedisSession {
    pub async fn open(cfg: &ConnectionConfig, password: Option<&str>, database: &str) -> AppResult<(RedisSession, ServerInfo)> {
        let host = if cfg.host.trim().is_empty() { "127.0.0.1".to_string() } else { cfg.host.trim().to_string() };
        let mode = cfg.options.redis_mode();
        let port = match cfg.port {
            0 if mode == "sentinel" => 26379,
            0 => 6379,
            p => p,
        };
        let db: i64 = database.trim().parse().unwrap_or(0);
        let tls = cfg.ssl_mode == SslMode::Require;
        let addr_of = |host: String, port: u16| {
            if tls {
                // Encrypted, certificate not verified (same as the SQL drivers).
                ConnectionAddr::TcpTls { host, port, insecure: true, tls_params: None }
            } else {
                ConnectionAddr::Tcp(host, port)
            }
        };
        let user = cfg.username.trim();
        let password = password.filter(|p| !p.is_empty());
        let node_info = |addr: ConnectionAddr, db: i64| -> AppResult<redis::ConnectionInfo> {
            let info = addr.into_connection_info().map_err(redis_error)?;
            let mut settings = info.redis_settings().clone().set_db(db);
            if !user.is_empty() {
                settings = settings.set_username(user);
            }
            if let Some(pw) = password {
                settings = settings.set_password(pw);
            }
            Ok(info.set_redis_settings(settings))
        };
        let config = AsyncConnectionConfig::new()
            .set_connection_timeout(Some(Duration::from_secs(10)))
            .set_response_timeout(Some(Duration::from_secs(60)));
        let mut read_only = cfg.read_only;
        let (conn, db) = match mode {
            "cluster" => {
                if db != 0 {
                    return Err(AppError::invalid("Redis Cluster only has database 0"));
                }
                let mut seeds = vec![node_info(addr_of(host.clone(), port), 0)?];
                for (h, p) in cfg.options.nodes(6379) {
                    seeds.push(node_info(addr_of(h, p), 0)?);
                }
                let mut b = redis::cluster::ClusterClientBuilder::new(seeds)
                    .connection_timeout(Duration::from_secs(10))
                    .response_timeout(Duration::from_secs(60))
                    .retries(2);
                if tls {
                    b = b.tls(TlsMode::Insecure);
                }
                let client = b.build().map_err(redis_error)?;
                let conn = tokio::time::timeout(Duration::from_secs(30), client.get_async_connection())
                    .await
                    .map_err(|_| AppError::new("connection", "Timed out while connecting to the cluster"))?
                    .map_err(cluster_error)?;
                (Conn::Cluster(conn), 0)
            }
            "sentinel" => {
                let master = cfg.options.sentinel_master.trim();
                let master = if master.is_empty() { "mymaster" } else { master };
                let mut sentinels = vec![ConnectionAddr::Tcp(host.clone(), port)];
                for (h, p) in cfg.options.nodes(26379) {
                    sentinels.push(ConnectionAddr::Tcp(h, p));
                }
                let replica = cfg.options.redis_replica;
                let kind = if replica { SentinelServerType::Replica } else { SentinelServerType::Master };
                let mut b = SentinelClientBuilder::new(sentinels, master, kind).map_err(redis_error)?.set_client_to_redis_db(db);
                if !user.is_empty() {
                    b = b.set_client_to_redis_username(user);
                }
                if let Some(pw) = password {
                    b = b.set_client_to_redis_password(pw);
                }
                let s_user = cfg.options.sentinel_user.trim();
                if !s_user.is_empty() {
                    b = b.set_client_to_sentinel_username(s_user);
                }
                if let Some(pw) = cfg.sentinel_password.as_deref().filter(|p| !p.is_empty()) {
                    b = b.set_client_to_sentinel_password(pw);
                }
                if tls {
                    b = b.set_client_to_redis_tls_mode(TlsMode::Insecure).set_client_to_sentinel_tls_mode(TlsMode::Insecure);
                }
                let mut client = b.build().map_err(redis_error)?;
                let conn = tokio::time::timeout(Duration::from_secs(30), client.get_async_connection_with_config(&config))
                    .await
                    .map_err(|_| AppError::new("connection", "Timed out while asking the Sentinels for the server"))?
                    .map_err(|e| sentinel_error(e, master))?;
                // Replicas reject writes anyway; say so up front.
                read_only |= replica;
                (Conn::Single(conn), db)
            }
            _ => {
                let client = redis::Client::open(node_info(addr_of(host.clone(), port), db)?).map_err(redis_error)?;
                let conn = client
                    .get_multiplexed_async_connection_with_config(&config)
                    .await
                    .map_err(redis_error)?;
                (Conn::Single(conn), db)
            }
        };
        let session = RedisSession { conn, db, read_only };
        let server = session.one_node::<String>(cmd("INFO").arg("server").clone()).await.unwrap_or_default();
        let field = |name: &str| {
            server
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{name}:")))
                .map(|v| v.trim().to_string())
        };
        let flavor = if let Some(v) = field("valkey_version") {
            ("Valkey", v)
        } else if field("dragonfly_version").is_some() {
            ("Dragonfly", field("dragonfly_version").unwrap_or_default())
        } else if server.contains("keydb") {
            ("KeyDB", field("redis_version").unwrap_or_default())
        } else {
            ("Redis", field("redis_version").unwrap_or_default())
        };
        let info = ServerInfo {
            session_id: String::new(),
            connection_id: cfg.id.clone(),
            engine: Engine::Redis,
            flavor: flavor.0.into(),
            version: flavor.1,
            database: db.to_string(),
            default_schema: String::new(),
            user: if cfg.username.trim().is_empty() { "default".into() } else { cfg.username.trim().into() },
            read_only,
        };
        Ok((session, info))
    }

    pub fn is_cluster(&self) -> bool {
        matches!(self.conn, Conn::Cluster(_))
    }

    async fn query<T: redis::FromRedisValue>(&self, c: &mut redis::Cmd) -> AppResult<T> {
        let mut conn = self.conn.clone();
        c.query_async(&mut conn).await.map_err(redis_error)
    }

    /// Runs a server-level command (INFO, CLUSTER NODES…) on one node. On a
    /// cluster those would otherwise fan out to every node.
    async fn one_node<T: redis::FromRedisValue>(&self, c: Cmd) -> AppResult<T> {
        match &self.conn {
            Conn::Single(_) => self.query(&mut c.clone()).await,
            Conn::Cluster(cc) => {
                let v = cc
                    .clone()
                    .route_command(c, RoutingInfo::SingleNode(SingleNodeRoutingInfo::Random))
                    .await
                    .map_err(redis_error)?;
                redis::from_redis_value(v).map_err(|e| AppError::new("db", e.to_string()))
            }
        }
    }

    /// Runs a command on a specific cluster node.
    async fn on_node(&self, c: Cmd, host: &str, port: u16) -> AppResult<Value> {
        match &self.conn {
            Conn::Cluster(cc) => cc
                .clone()
                .route_command(
                    c,
                    RoutingInfo::SingleNode(SingleNodeRoutingInfo::ByAddress { host: host.to_string(), port }),
                )
                .await
                .map_err(redis_error),
            Conn::Single(_) => self.query(&mut c.clone()).await,
        }
    }

    /// The cluster's nodes, sorted by address.
    pub async fn cluster_nodes(&self) -> AppResult<Vec<ClusterNode>> {
        let text: String = self.one_node(cmd("CLUSTER").arg("NODES").clone()).await?;
        Ok(parse_cluster_nodes(&text))
    }

    async fn primaries(&self) -> AppResult<Vec<ClusterNode>> {
        let nodes: Vec<ClusterNode> = self.cluster_nodes().await?.into_iter().filter(|n| n.primary && !n.failed).collect();
        if nodes.is_empty() {
            return Err(AppError::new("db", "The cluster reported no reachable primary nodes"));
        }
        Ok(nodes)
    }

    fn guard_write(&self) -> AppResult<()> {
        if self.read_only {
            return Err(AppError::invalid("This connection is read-only, so Rowglow blocked the change.").with_code("read_only"));
        }
        Ok(())
    }

    pub async fn databases(&self) -> AppResult<Vec<String>> {
        if self.is_cluster() {
            return Ok(vec!["0".into()]);
        }
        let count = match self.query::<Vec<String>>(&mut cmd("CONFIG").arg("GET").arg("databases").clone()).await {
            Ok(v) => v.get(1).and_then(|n| n.parse::<i64>().ok()).unwrap_or(16),
            Err(_) => 16, // managed Redis often disables CONFIG
        };
        Ok((0..count.clamp(1, 256)).map(|i| i.to_string()).collect())
    }

    /// Server overview: version, memory, and keys per database.
    pub async fn overview(&self) -> AppResult<Json> {
        if self.is_cluster() {
            return self.cluster_overview().await;
        }
        let info: String = self.query(&mut cmd("INFO").clone()).await?;
        let mut sections = serde_json::Map::new();
        let mut keyspace = Vec::new();
        for line in info.lines() {
            if let Some((k, v)) = line.split_once(':') {
                if let Some(db) = k.strip_prefix("db") {
                    let keys = v.split(',').find_map(|p| p.strip_prefix("keys=")).and_then(|n| n.parse::<i64>().ok()).unwrap_or(0);
                    keyspace.push(json!({ "db": db.parse::<i64>().unwrap_or(0), "keys": keys }));
                } else if matches!(
                    k,
                    "redis_version" | "valkey_version" | "redis_mode" | "os" | "uptime_in_days" | "connected_clients"
                        | "used_memory_human" | "maxmemory_human" | "maxmemory_policy" | "role" | "total_commands_processed"
                ) {
                    sections.insert(k.to_string(), json!(v.trim()));
                }
            }
        }
        let dbsize: i64 = self.query(&mut cmd("DBSIZE").clone()).await.unwrap_or(0);
        Ok(json!({ "info": sections, "keyspace": keyspace, "dbsize": dbsize, "db": self.db }))
    }

    async fn cluster_overview(&self) -> AppResult<Json> {
        let info: String = self.one_node(cmd("INFO").clone()).await?;
        let mut sections = serde_json::Map::new();
        for line in info.lines() {
            if let Some((k, v)) = line.split_once(':') {
                if matches!(k, "redis_version" | "valkey_version" | "os" | "uptime_in_days" | "maxmemory_policy") {
                    sections.insert(k.to_string(), json!(v.trim()));
                }
            }
        }
        sections.insert("redis_mode".into(), json!("cluster"));
        let cinfo: String = self.one_node(cmd("CLUSTER").arg("INFO").clone()).await.unwrap_or_default();
        let cfield = |name: &str| cinfo.lines().find_map(|l| l.strip_prefix(&format!("{name}:"))).map(|v| v.trim().to_string());
        let nodes = self.cluster_nodes().await?;
        let mut rows = Vec::new();
        let mut total = 0i64;
        let jobs = nodes.iter().filter(|n| !n.failed).map(|n| async move {
            let keys = self.on_node(cmd("DBSIZE").clone(), &n.host, n.port).await.map(|v| int_of(&v)).ok();
            let mem = self
                .on_node(cmd("INFO").arg("memory").clone(), &n.host, n.port)
                .await
                .ok()
                .map(|v| String::from_utf8_lossy(&bytes_of(&v)).into_owned())
                .and_then(|t| t.lines().find_map(|l| l.strip_prefix("used_memory_human:")).map(|v| v.trim().to_string()));
            (n.clone(), keys, mem)
        });
        for (n, keys, mem) in futures_util::future::join_all(jobs).await {
            if n.primary {
                total += keys.unwrap_or(0);
            }
            rows.push(json!({
                "address": format!("{}:{}", n.host, n.port),
                "role": if n.primary { "primary" } else { "replica" },
                "slots": n.slots,
                "keys": keys,
                "memory": mem,
            }));
        }
        for n in nodes.iter().filter(|n| n.failed) {
            rows.push(json!({ "address": format!("{}:{}", n.host, n.port), "role": "failed", "slots": n.slots, "keys": null, "memory": null }));
        }
        Ok(json!({
            "info": sections,
            "keyspace": [{ "db": 0, "keys": total }],
            "dbsize": total,
            "db": 0,
            "cluster": {
                "state": cfield("cluster_state"),
                "size": cfield("cluster_size"),
                "knownNodes": cfield("cluster_known_nodes"),
                "slotsAssigned": cfield("cluster_slots_assigned"),
                "nodes": rows,
            },
        }))
    }

    /// Cluster SCAN: walks the primaries one after another. The cursor is
    /// `<primary index>:<that node's cursor>`, and "0" when every node is done.
    async fn cluster_scan(&self, cursor: &str, pattern: &str, count: usize, kind: Option<&str>) -> AppResult<Json> {
        let primaries = self.primaries().await?;
        let (mut idx, mut node_cursor) = parse_cluster_cursor(cursor);
        let mut keys: Vec<Vec<u8>> = Vec::new();
        let mut node: Option<&ClusterNode> = None;
        // Keep going until this page has keys or we run out of nodes, so an
        // empty node doesn't end the listing early.
        while idx < primaries.len() {
            let n = &primaries[idx];
            let mut c = cmd("SCAN");
            c.arg(node_cursor)
                .arg("MATCH")
                .arg(if pattern.trim().is_empty() { "*" } else { pattern })
                .arg("COUNT")
                .arg(count.clamp(10, 5000));
            if let Some(t) = kind.filter(|t| !t.is_empty()) {
                c.arg("TYPE").arg(t);
            }
            let reply = self.on_node(c, &n.host, n.port).await?;
            let mut parts = items_of(reply).into_iter();
            let next: u64 = parts.next().map(|v| String::from_utf8_lossy(&bytes_of(&v)).into_owned()).and_then(|c| c.parse().ok()).unwrap_or(0);
            keys = parts.next().map(items_of).unwrap_or_default().iter().map(bytes_of).collect();
            node = Some(n);
            if next == 0 {
                idx += 1;
                node_cursor = 0;
            } else {
                node_cursor = next;
            }
            if !keys.is_empty() {
                break;
            }
        }
        let next = if idx >= primaries.len() { "0".to_string() } else { format!("{idx}:{node_cursor}") };
        let mut out = Vec::with_capacity(keys.len());
        if let (Some(n), false) = (node, keys.is_empty()) {
            // Every key from one node's SCAN lives on that node, so one
            // pipeline to it fetches all their types and TTLs.
            let mut pipe = redis::pipe();
            for k in &keys {
                pipe.cmd("TYPE").arg(k).cmd("PTTL").arg(k);
            }
            let meta: Vec<Value> = match &self.conn {
                Conn::Cluster(cc) => cc
                    .clone()
                    .route_pipeline(pipe, 0, keys.len() * 2, SingleNodeRoutingInfo::ByAddress { host: n.host.clone(), port: n.port })
                    .await
                    .map_err(redis_error)?,
                Conn::Single(_) => unreachable!("cluster scan on a single server"),
            };
            for (i, k) in keys.iter().enumerate() {
                let t = meta.get(i * 2).map(|v| String::from_utf8_lossy(&bytes_of(v)).into_owned()).unwrap_or_default();
                let ttl = meta.get(i * 2 + 1).map(int_of).unwrap_or(-1);
                let mut key = text_or_b64(k);
                key["type"] = json!(t);
                key["ttl"] = json!(ttl);
                out.push(key);
            }
        }
        Ok(json!({ "cursor": next, "keys": out }))
    }

    pub async fn scan(&self, cursor: &str, pattern: &str, count: usize, kind: Option<&str>) -> AppResult<Json> {
        if self.is_cluster() {
            return self.cluster_scan(cursor, pattern, count, kind).await;
        }
        let mut c = cmd("SCAN");
        c.arg(cursor.parse::<u64>().unwrap_or(0))
            .arg("MATCH")
            .arg(if pattern.trim().is_empty() { "*" } else { pattern })
            .arg("COUNT")
            .arg(count.clamp(10, 5000));
        if let Some(t) = kind.filter(|t| !t.is_empty()) {
            c.arg("TYPE").arg(t);
        }
        let reply: Value = self.query(&mut c).await?;
        let mut parts = items_of(reply).into_iter();
        let next = parts.next().map(|v| String::from_utf8_lossy(&bytes_of(&v)).into_owned()).unwrap_or_else(|| "0".into());
        let keys: Vec<Vec<u8>> = parts.next().map(items_of).unwrap_or_default().iter().map(bytes_of).collect();
        let mut out = Vec::with_capacity(keys.len());
        if !keys.is_empty() {
            let mut pipe = redis::pipe();
            for k in &keys {
                pipe.cmd("TYPE").arg(k).cmd("PTTL").arg(k);
            }
            let mut conn = self.conn.clone();
            let meta: Vec<Value> = pipe.query_async(&mut conn).await.map_err(redis_error)?;
            for (i, k) in keys.iter().enumerate() {
                let t = meta.get(i * 2).map(|v| String::from_utf8_lossy(&bytes_of(v)).into_owned()).unwrap_or_default();
                let ttl = meta.get(i * 2 + 1).map(int_of).unwrap_or(-1);
                let mut key = text_or_b64(k);
                key["type"] = json!(t);
                key["ttl"] = json!(ttl);
                out.push(key);
            }
        }
        Ok(json!({ "cursor": next, "keys": out }))
    }

    pub async fn get(&self, key: &str, key_b64: &Option<String>) -> AppResult<Json> {
        let k = key_bytes(key, key_b64)?;
        let kind: String = self.query(&mut cmd("TYPE").arg(&k).clone()).await?;
        let ttl: i64 = self.query(&mut cmd("PTTL").arg(&k).clone()).await?;
        if kind == "none" {
            return Err(AppError::not_found("This key no longer exists (it expired or was deleted).").with_code("key_missing"));
        }
        let mut out = json!({ "type": kind, "ttl": ttl });
        match kind.as_str() {
            "string" => {
                let len: i64 = self.query(&mut cmd("STRLEN").arg(&k).clone()).await?;
                let v: Vec<u8> = self.query(&mut cmd("GETRANGE").arg(&k).arg(0).arg(MAX_STRING as i64 - 1).clone()).await?;
                out["length"] = json!(len);
                out["value"] = text_or_b64(&v);
                out["truncated"] = json!(len as usize > MAX_STRING);
            }
            "hash" => {
                let len: i64 = self.query(&mut cmd("HLEN").arg(&k).clone()).await?;
                let items = self.scan_all("HSCAN", &k).await?;
                out["length"] = json!(len);
                out["items"] = json!(items
                    .chunks(2)
                    .map(|p| json!({ "field": text_or_b64(&p[0]), "value": text_or_b64(p.get(1).map(Vec::as_slice).unwrap_or_default()) }))
                    .collect::<Vec<_>>());
                out["truncated"] = json!(len as usize > MAX_ITEMS);
            }
            "list" => {
                let len: i64 = self.query(&mut cmd("LLEN").arg(&k).clone()).await?;
                let v: Vec<Vec<u8>> = self.query(&mut cmd("LRANGE").arg(&k).arg(0).arg(MAX_ITEMS as i64 - 1).clone()).await?;
                out["length"] = json!(len);
                out["items"] = json!(v.iter().enumerate().map(|(i, x)| json!({ "index": i, "value": text_or_b64(x) })).collect::<Vec<_>>());
                out["truncated"] = json!(len as usize > MAX_ITEMS);
            }
            "set" => {
                let len: i64 = self.query(&mut cmd("SCARD").arg(&k).clone()).await?;
                let items = self.scan_all("SSCAN", &k).await?;
                out["length"] = json!(len);
                out["items"] = json!(items.iter().map(|m| json!({ "member": text_or_b64(m) })).collect::<Vec<_>>());
                out["truncated"] = json!(len as usize > MAX_ITEMS);
            }
            "zset" => {
                let len: i64 = self.query(&mut cmd("ZCARD").arg(&k).clone()).await?;
                let v: Vec<Vec<u8>> = self
                    .query(&mut cmd("ZRANGE").arg(&k).arg(0).arg(MAX_ITEMS as i64 - 1).arg("WITHSCORES").clone())
                    .await?;
                out["length"] = json!(len);
                out["items"] = json!(v
                    .chunks(2)
                    .map(|p| json!({ "member": text_or_b64(&p[0]), "score": String::from_utf8_lossy(p.get(1).map(Vec::as_slice).unwrap_or_default()) }))
                    .collect::<Vec<_>>());
                out["truncated"] = json!(len as usize > MAX_ITEMS);
            }
            "stream" => {
                let len: i64 = self.query(&mut cmd("XLEN").arg(&k).clone()).await?;
                let v: Value = self.query(&mut cmd("XREVRANGE").arg(&k).arg("+").arg("-").arg("COUNT").arg(MAX_ITEMS).clone()).await?;
                let entries: Vec<Json> = items_of(v)
                    .into_iter()
                    .map(|e| {
                        let mut parts = items_of(e).into_iter();
                        let id = parts.next().map(|v| String::from_utf8_lossy(&bytes_of(&v)).into_owned()).unwrap_or_default();
                        let fields: Vec<Json> = parts
                            .next()
                            .map(items_of)
                            .unwrap_or_default()
                            .chunks(2)
                            .map(|p| json!([text_or_b64(&bytes_of(&p[0])), text_or_b64(&p.get(1).map(bytes_of).unwrap_or_default())]))
                            .collect();
                        json!({ "id": id, "fields": fields })
                    })
                    .collect();
                out["length"] = json!(len);
                out["items"] = json!(entries);
                out["truncated"] = json!(len as usize > MAX_ITEMS);
            }
            "ReJSON-RL" => {
                let v: Option<String> = self.query(&mut cmd("JSON.GET").arg(&k).clone()).await?;
                out["value"] = json!({ "text": v.unwrap_or_default() });
            }
            other => {
                out["unsupported"] = json!(format!("Rowglow can't show {other} keys yet. Use the console."));
            }
        }
        Ok(out)
    }

    /// HSCAN / SSCAN until MAX_ITEMS entries (fields+values count as one).
    async fn scan_all(&self, command: &str, key: &[u8]) -> AppResult<Vec<Vec<u8>>> {
        let per = if command == "HSCAN" { 2 } else { 1 };
        let mut cursor = 0u64;
        let mut out: Vec<Vec<u8>> = Vec::new();
        loop {
            let reply: Value = self.query(&mut cmd(command).arg(key).arg(cursor).arg("COUNT").arg(500).clone()).await?;
            let mut parts = items_of(reply).into_iter();
            cursor = parts.next().map(|v| int_of(&v) as u64).unwrap_or(0);
            out.extend(parts.next().map(items_of).unwrap_or_default().iter().map(bytes_of));
            if cursor == 0 || out.len() / per >= MAX_ITEMS {
                break;
            }
        }
        out.truncate(MAX_ITEMS * per);
        Ok(out)
    }

    pub async fn op(&self, op: RedisOp) -> AppResult<Json> {
        self.guard_write()?;
        let changed = |n: i64| json!({ "ok": true, "changed": n });
        match op {
            RedisOp::SetString { key, key_b64, value } => {
                let k = key_bytes(&key, &key_b64)?;
                // KEEPTTL keeps an existing expiry (Redis 6+); XX never creates a key.
                let r: Value = self.query(&mut cmd("SET").arg(&k).arg(value).arg("KEEPTTL").arg("XX").clone()).await?;
                if matches!(r, Value::Nil) {
                    return Err(AppError::not_found("This key no longer exists (it expired or was deleted).").with_code("key_missing"));
                }
                Ok(changed(1))
            }
            RedisOp::HashSet { key, key_b64, field, value, old_field } => {
                let k = key_bytes(&key, &key_b64)?;
                let mut pipe = redis::pipe();
                pipe.atomic();
                if let Some(old) = old_field.filter(|o| *o != field) {
                    pipe.cmd("HDEL").arg(&k).arg(old).ignore();
                }
                pipe.cmd("HSET").arg(&k).arg(field).arg(value).ignore();
                let mut conn = self.conn.clone();
                let _: () = pipe.query_async(&mut conn).await.map_err(redis_error)?;
                Ok(changed(1))
            }
            RedisOp::HashDel { key, key_b64, fields } => {
                let k = key_bytes(&key, &key_b64)?;
                let n: i64 = self.query(&mut cmd("HDEL").arg(&k).arg(fields).clone()).await?;
                Ok(changed(n))
            }
            RedisOp::ListSet { key, key_b64, index, value, expected } => {
                let k = key_bytes(&key, &key_b64)?;
                if let Some(exp) = expected {
                    let cur: Option<String> = self.query(&mut cmd("LINDEX").arg(&k).arg(index).clone()).await?;
                    if cur.as_deref() != Some(exp.as_str()) {
                        return Err(AppError::new("db", "This list changed since it was loaded. Reload it and try again.").with_code("row_not_found"));
                    }
                }
                let _: Value = self.query(&mut cmd("LSET").arg(&k).arg(index).arg(value).clone()).await?;
                Ok(changed(1))
            }
            RedisOp::ListPush { key, key_b64, value, head } => {
                let k = key_bytes(&key, &key_b64)?;
                let n: i64 = self.query(&mut cmd(if head { "LPUSHX" } else { "RPUSHX" }).arg(&k).arg(value).clone()).await?;
                Ok(changed(n))
            }
            RedisOp::ListRemove { key, key_b64, index, expected } => {
                let k = key_bytes(&key, &key_b64)?;
                let cur: Option<String> = self.query(&mut cmd("LINDEX").arg(&k).arg(index).clone()).await?;
                if cur.as_deref() != Some(expected.as_str()) {
                    return Err(AppError::new("db", "This list changed since it was loaded. Reload it and try again.").with_code("row_not_found"));
                }
                // Mark the element, then remove exactly that mark.
                let mark = format!("__rowglow_delete_{}", crate::util::new_id());
                let mut pipe = redis::pipe();
                pipe.atomic().cmd("LSET").arg(&k).arg(index).arg(&mark).ignore().cmd("LREM").arg(&k).arg(1).arg(&mark).ignore();
                let mut conn = self.conn.clone();
                let _: () = pipe.query_async(&mut conn).await.map_err(redis_error)?;
                Ok(changed(1))
            }
            RedisOp::SetAdd { key, key_b64, members } => {
                let k = key_bytes(&key, &key_b64)?;
                let n: i64 = self.query(&mut cmd("SADD").arg(&k).arg(members).clone()).await?;
                Ok(changed(n))
            }
            RedisOp::SetRemove { key, key_b64, members } => {
                let k = key_bytes(&key, &key_b64)?;
                let n: i64 = self.query(&mut cmd("SREM").arg(&k).arg(members).clone()).await?;
                Ok(changed(n))
            }
            RedisOp::ZsetAdd { key, key_b64, member, score } => {
                let k = key_bytes(&key, &key_b64)?;
                let n: i64 = self.query(&mut cmd("ZADD").arg(&k).arg(score).arg(member).clone()).await?;
                Ok(changed(n))
            }
            RedisOp::ZsetRemove { key, key_b64, members } => {
                let k = key_bytes(&key, &key_b64)?;
                let n: i64 = self.query(&mut cmd("ZREM").arg(&k).arg(members).clone()).await?;
                Ok(changed(n))
            }
            RedisOp::Create { key, kind, value, field, score, ttl } => {
                if key.is_empty() {
                    return Err(AppError::invalid("Enter a key name"));
                }
                let exists: i64 = self.query(&mut cmd("EXISTS").arg(&key).clone()).await?;
                if exists > 0 {
                    return Err(AppError::invalid(format!("The key {key} already exists")));
                }
                let mut c = match kind.as_str() {
                    "string" => cmd("SET").arg(&key).arg(&value).arg("NX").clone(),
                    "hash" => cmd("HSET").arg(&key).arg(field.unwrap_or_else(|| "field".into())).arg(&value).clone(),
                    "list" => cmd("RPUSH").arg(&key).arg(&value).clone(),
                    "set" => cmd("SADD").arg(&key).arg(&value).clone(),
                    "zset" => cmd("ZADD").arg(&key).arg(score.unwrap_or(0.0)).arg(&value).clone(),
                    other => return Err(AppError::invalid(format!("Can't create {other} keys"))),
                };
                let _: Value = self.query(&mut c).await?;
                if let Some(t) = ttl.filter(|t| *t > 0) {
                    let _: i64 = self.query(&mut cmd("EXPIRE").arg(&key).arg(t).clone()).await?;
                }
                Ok(changed(1))
            }
            RedisOp::Expire { key, key_b64, ttl } => {
                let k = key_bytes(&key, &key_b64)?;
                let n: i64 = match ttl {
                    Some(t) if t > 0 => self.query(&mut cmd("EXPIRE").arg(&k).arg(t).clone()).await?,
                    _ => self.query(&mut cmd("PERSIST").arg(&k).clone()).await?,
                };
                Ok(changed(n))
            }
            RedisOp::Rename { key, key_b64, new_key } => {
                let k = key_bytes(&key, &key_b64)?;
                if new_key.is_empty() {
                    return Err(AppError::invalid("Enter the new key name"));
                }
                let n: i64 = self.query(&mut cmd("RENAMENX").arg(&k).arg(&new_key).clone()).await?;
                if n == 0 {
                    return Err(AppError::invalid(format!("A key named {new_key} already exists, so nothing was renamed.")));
                }
                Ok(changed(1))
            }
            RedisOp::Delete { keys } => {
                if keys.is_empty() {
                    return Ok(changed(0));
                }
                let keys: Vec<Vec<u8>> = keys.iter().map(|k| key_bytes(k, &None)).collect::<AppResult<_>>()?;
                let n = match self.query::<i64>(&mut cmd("UNLINK").arg(&keys).clone()).await {
                    Ok(n) => n,
                    Err(_) => self.query::<i64>(&mut cmd("DEL").arg(&keys).clone()).await?,
                };
                Ok(changed(n))
            }
        }
    }

    pub async fn delete_raw(&self, key_b64: &str) -> AppResult<i64> {
        self.guard_write()?;
        let k = key_bytes("", &Some(key_b64.to_string()))?;
        self.query(&mut cmd("UNLINK").arg(&k).clone()).await
    }

    /// Console: runs one command line with the safety rules above.
    pub async fn command(&self, line: &str, confirmed: bool) -> AppResult<Json> {
        let raw = split_args_bytes(line)?;
        let args: Vec<String> = raw.iter().map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        let Some(name) = args.first().map(|a| a.to_ascii_uppercase()) else {
            return Err(AppError::invalid("Type a command, e.g. GET mykey"));
        };
        if REFUSED.contains(&name.as_str()) {
            let why = match name.as_str() {
                "SELECT" => "Use the database picker to switch databases.",
                "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE" | "MONITOR" => "Streaming commands aren't supported in the console yet.",
                "AUTH" => "Edit the connection to change credentials.",
                _ => "It would block or take over the shared connection.",
            };
            return Err(AppError::invalid(format!("{name} can't run in Rowglow's console. {why}")).with_code("refused"));
        }
        if self.read_only && !is_read_command(&args) {
            return Err(AppError::invalid(format!("This connection is read-only, so Rowglow blocked {name}.")).with_code("read_only"));
        }
        if !confirmed {
            if let Some(reason) = danger_reason(&args) {
                let mut e = AppError::invalid(reason).with_code("confirm_required");
                e.detail = Some(line.to_string());
                return Err(e);
            }
        }
        let start = std::time::Instant::now();
        let mut c = cmd(&args[0]);
        for a in &raw[1..] {
            c.arg(a.as_slice());
        }
        let mut conn = self.conn.clone();
        let reply: Result<Value, _> = c.query_async(&mut conn).await;
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        match reply {
            Ok(v) => Ok(json!({ "reply": reply_json(&v), "durationMs": ms, "write": !is_read_command(&args) })),
            Err(e) if e.is_io_error() || e.is_connection_dropped() => Err(redis_error(e)),
            Err(e) => Ok(json!({ "reply": { "t": "error", "v": e.to_string() }, "durationMs": ms })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_redis_cli() {
        assert_eq!(split_args(r#"SET "my key" 'it''s' plain"#).unwrap(), ["SET", "my key", "its", "plain"]);
        assert_eq!(split_args(r#"SET k "a\nb\x41\"""#).unwrap(), ["SET", "k", "a\nbA\""]);
        assert_eq!(split_args(r#"SET k """#).unwrap(), ["SET", "k", ""]);
        assert_eq!(split_args_bytes(r#"SET k "\xff\x00""#).unwrap()[2], vec![0xff, 0x00]);
        assert!(split_args(r#"GET "oops"#).is_err());
    }

    #[test]
    fn classifies_commands() {
        let a = |s: &str| split_args(s).unwrap();
        assert!(is_read_command(&a("get k")));
        assert!(is_read_command(&a("CONFIG GET maxmemory")));
        assert!(!is_read_command(&a("CONFIG SET maxmemory 1")));
        assert!(!is_read_command(&a("SET k v")));
        assert!(!is_read_command(&a("EVAL 'return 1' 0")));
        assert!(danger_reason(&a("flushall")).is_some());
        assert!(danger_reason(&a("KEYS *")).is_some());
        assert!(danger_reason(&a("GET k")).is_none());
    }
}
