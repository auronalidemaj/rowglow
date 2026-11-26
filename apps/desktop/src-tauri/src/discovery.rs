//! Local Radar — finds databases running on this machine:
//! * probes common Postgres / MySQL / SQL Server / Redis / MongoDB ports on
//!   127.0.0.1 and fingerprints the wire protocol with a pre-login handshake:
//!   MySQL's greeting packet, Postgres' SSLRequest reply, SQL Server's
//!   PRELOGIN reply, Redis' answer to PING and MongoDB's answer to buildInfo
//!   (both of which servers answer without credentials),
//! * identifies which program owns the port (Homebrew, Postgres.app, Docker…),
//! * reads running Docker containers (image, published ports and the
//!   POSTGRES_* / MYSQL_* / MARIADB_* / MSSQL_SA_PASSWORD / MONGO_INITDB_* /
//!   REDIS_PASSWORD env vars and `--requirepass`) to prefill credentials,
//! * never logs in on its own — only when the user clicks Connect.

use crate::model::Engine;
use crate::util::os_user;
use serde::Serialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovered {
    pub id: String,
    pub engine: Engine,
    pub flavor: String,
    pub version: Option<String>,
    pub host: String,
    pub port: u16,
    pub source: String,
    pub container: Option<String>,
    pub image: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub database: Option<String>,
    pub auth_ok: Option<bool>,
    pub auth_error: Option<String>,
    pub already_saved: bool,
    /// Extra detail from the handshake, e.g. "Password required" or
    /// "Replica set rs0".
    pub note: Option<String>,
}

const PG_PORTS: &[u16] = &[5432, 5433, 5434, 5435, 5436, 5437, 5438, 5439, 5440, 54320, 54321, 54322];
const MY_PORTS: &[u16] = &[3306, 3307, 3308, 3309, 3310, 3316, 3326];
const MS_PORTS: &[u16] = &[1433, 1401, 1402, 11433, 14330, 14333];
const REDIS_PORTS: &[u16] = &[6379, 6380, 6381, 6382, 6383, 6390, 16379];
const MONGO_PORTS: &[u16] = &[27017, 27018, 27019, 27020];

struct Fingerprint {
    engine: Engine,
    flavor: String,
    version: Option<String>,
    note: Option<String>,
}

impl Fingerprint {
    fn new(engine: Engine, flavor: &str, version: Option<String>) -> Self {
        Fingerprint { engine, flavor: flavor.into(), version, note: None }
    }
}

async fn open(port: u16) -> Option<TcpStream> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    timeout(Duration::from_millis(350), TcpStream::connect(addr)).await.ok()?.ok()
}

/// Reads until `n` bytes arrived, the peer closed, or 600 ms passed.
async fn read_some(stream: &mut TcpStream, want: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + Duration::from_millis(600);
    while out.len() < want {
        match tokio::time::timeout_at(deadline, stream.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => out.extend_from_slice(&buf[..n]),
            _ => break,
        }
    }
    out
}

/// Redis answers PING with +PONG, or -NOAUTH when a password is set. PING
/// doesn't sign in and changes nothing.
async fn fingerprint_redis(port: u16) -> Option<Fingerprint> {
    let mut stream = open(port).await?;
    stream.write_all(b"PING\r\n").await.ok()?;
    let reply = read_some(&mut stream, 7).await;
    let text = String::from_utf8_lossy(&reply);
    let mut fp = Fingerprint::new(Engine::Redis, "Redis", None);
    if text.starts_with("+PONG") {
        Some(fp)
    } else if text.starts_with("-NOAUTH") || text.starts_with("-WRONGPASS") || text.contains("Authentication required") {
        fp.note = Some("Password required".into());
        Some(fp)
    } else {
        None
    }
}

/// SQL Server: a TDS PRELOGIN packet (sent before any login) returns the
/// server version.
pub fn prelogin_packet() -> Vec<u8> {
    // Options: VERSION, ENCRYPTION, TERMINATOR. Offsets are from the payload start.
    let opts_len = 5 * 2 + 1; // two 5-byte option headers + terminator
    let mut payload = vec![
        0x00, 0x00, opts_len as u8, 0x00, 0x06, // VERSION at opts_len, 6 bytes
        0x01, 0x00, opts_len as u8 + 6, 0x00, 0x01, // ENCRYPTION, 1 byte
        0xFF,
    ];
    payload.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // client version (unused)
    payload.push(0x02); // ENCRYPT_NOT_SUP
    let len = (8 + payload.len()) as u16;
    let mut pkt = vec![0x12, 0x01, (len >> 8) as u8, len as u8, 0, 0, 1, 0];
    pkt.extend_from_slice(&payload);
    pkt
}

/// Server version from a PRELOGIN reply, e.g. "16.0.4135".
pub fn parse_prelogin(reply: &[u8]) -> Option<String> {
    if reply.len() < 8 || reply[0] != 0x04 {
        return None;
    }
    let body = &reply[8..];
    let mut i = 0;
    while i + 5 <= body.len() && body[i] != 0xFF {
        let token = body[i];
        let off = u16::from_be_bytes([body[i + 1], body[i + 2]]) as usize;
        let len = u16::from_be_bytes([body[i + 3], body[i + 4]]) as usize;
        if token == 0x00 && len >= 4 && off + 4 <= body.len() {
            let v = &body[off..off + 4];
            return Some(format!("{}.{}.{}", v[0], v[1], u16::from_be_bytes([v[2], v[3]])));
        }
        i += 5;
    }
    None
}

/// Marketing name for a SQL Server major version.
pub fn mssql_release(version: &str) -> Option<&'static str> {
    Some(match version.split('.').next()?.parse::<u32>().ok()? {
        17 => "2025",
        16 => "2022",
        15 => "2019",
        14 => "2017",
        13 => "2016",
        12 => "2014",
        11 => "2012",
        _ => return None,
    })
}

async fn fingerprint_mssql(port: u16) -> Option<Fingerprint> {
    let mut stream = open(port).await?;
    stream.write_all(&prelogin_packet()).await.ok()?;
    let reply = read_some(&mut stream, 8).await;
    if reply.len() >= 4 {
        let total = u16::from_be_bytes([reply[2], reply[3]]) as usize;
        let mut reply = reply;
        if reply.len() < total {
            reply.extend(read_some(&mut stream, total - reply.len()).await);
        }
        let version = parse_prelogin(&reply)?;
        let mut fp = Fingerprint::new(Engine::Mssql, "SQL Server", Some(version.clone()));
        fp.note = mssql_release(&version).map(|r| format!("SQL Server {r}"));
        return Some(fp);
    }
    None
}

/// MongoDB: buildInfo is one of the few commands a server answers before
/// authentication; it returns the version and nothing about the data.
pub fn mongo_build_info_msg() -> Vec<u8> {
    let doc = mongodb::bson::doc! { "buildInfo": 1, "$db": "admin" };
    let body = mongodb::bson::to_vec(&doc).unwrap_or_default();
    let len = (16 + 4 + 1 + body.len()) as i32;
    let mut m = Vec::with_capacity(len as usize);
    m.extend_from_slice(&len.to_le_bytes());
    m.extend_from_slice(&1i32.to_le_bytes()); // requestID
    m.extend_from_slice(&0i32.to_le_bytes()); // responseTo
    m.extend_from_slice(&2013i32.to_le_bytes()); // OP_MSG
    m.extend_from_slice(&0u32.to_le_bytes()); // flags
    m.push(0); // section kind 0: body
    m.extend_from_slice(&body);
    m
}

/// The reply document of an OP_MSG, if `reply` is one.
pub fn parse_op_msg(reply: &[u8]) -> Option<mongodb::bson::Document> {
    if reply.len() < 21 {
        return None;
    }
    let op = i32::from_le_bytes(reply[12..16].try_into().ok()?);
    if op != 2013 || reply[20] != 0 {
        return None;
    }
    mongodb::bson::Document::from_reader(&mut &reply[21..]).ok()
}

async fn fingerprint_mongo(port: u16) -> Option<Fingerprint> {
    let mut stream = open(port).await?;
    stream.write_all(&mongo_build_info_msg()).await.ok()?;
    let head = read_some(&mut stream, 4).await;
    if head.len() < 4 {
        return None;
    }
    let total = i32::from_le_bytes(head[0..4].try_into().ok()?).clamp(0, 1 << 20) as usize;
    let mut reply = head;
    if reply.len() < total {
        reply.extend(read_some(&mut stream, total - reply.len()).await);
    }
    let doc = parse_op_msg(&reply)?;
    let version = doc.get_str("version").ok().map(str::to_string);
    let mut fp = Fingerprint::new(Engine::Mongodb, "MongoDB", version);
    if doc.get_str("version").is_err() {
        fp.note = Some("Password required".into());
    }
    Some(fp)
}

async fn fingerprint(port: u16) -> Option<Fingerprint> {
    if REDIS_PORTS.contains(&port) {
        return fingerprint_redis(port).await;
    }
    if MONGO_PORTS.contains(&port) {
        return fingerprint_mongo(port).await;
    }
    if MS_PORTS.contains(&port) {
        return fingerprint_mssql(port).await;
    }
    let mut stream = open(port).await?;
    let mut buf = [0u8; 512];
    match timeout(Duration::from_millis(450), stream.read(&mut buf)).await {
        Ok(Ok(n)) if n > 5 => {
            // MySQL / MariaDB send a greeting packet first:
            // [3 bytes length][1 byte seq][protocol version = 10][server version\0]...
            if buf[4] == 10 {
                let rest = &buf[5..n];
                let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
                let raw = String::from_utf8_lossy(&rest[..end]).to_string();
                let maria = raw.to_lowercase().contains("mariadb");
                let version = raw
                    .trim_start_matches("5.5.5-")
                    .split('-')
                    .next()
                    .unwrap_or("")
                    .to_string();
                return Some(Fingerprint::new(Engine::Mysql, if maria { "MariaDB" } else { "MySQL" }, Some(version)));
            }
            if buf[4] == 0xff {
                // Error packet (e.g. "Host is not allowed to connect") — still MySQL.
                return Some(Fingerprint::new(Engine::Mysql, "MySQL", None));
            }
            None
        }
        Ok(Ok(_)) => None,
        _ => {
            // Silent server: try the Postgres SSLRequest handshake.
            let req: [u8; 8] = [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f];
            stream.write_all(&req).await.ok()?;
            let mut b = [0u8; 1];
            match timeout(Duration::from_millis(450), stream.read(&mut b)).await {
                Ok(Ok(1)) if b[0] == b'S' || b[0] == b'N' => Some(Fingerprint::new(Engine::Postgres, "PostgreSQL", None)),
                _ => None,
            }
        }
    }
}

/// Which program is listening on the port? (best effort, see `crate::os`)
async fn port_owner(port: u16) -> Option<String> {
    let path = crate::os::port_owner_path(port).await?;
    Some(owner_label(&path).to_string())
}

/// Maps the listening program's path to a friendly source label.
fn owner_label(path: &str) -> &'static str {
    let path = path.to_lowercase();
    if path.contains("postgres.app") {
        "Postgres.app"
    } else if path.contains("dbngin") {
        "DBngin"
    } else if path.contains("mamp") {
        "MAMP"
    } else if path.contains("herd") {
        "Laravel Herd"
    } else if path.contains("orbstack") {
        "OrbStack"
    } else if path.contains("docker") || path.contains("vpnkit") || path.contains("colima") || path.contains("lima") {
        "Docker"
    } else if path.contains("homebrew") || path.contains("/usr/local/cellar") || path.contains("/usr/local/opt") || path.contains("linuxbrew") {
        "Homebrew"
    } else if path.contains("laragon") {
        "Laragon"
    } else if path.contains("xampp") {
        "XAMPP"
    } else if path.contains("wamp") {
        "WampServer"
    } else if path.contains("wslrelay") || path.contains("wslhost") {
        "WSL"
    } else {
        "Local server"
    }
}

fn docker_bin() -> Option<PathBuf> {
    // GUI apps get a minimal PATH on macOS (and sometimes Linux), so check the
    // usual install locations before falling back to PATH.
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    candidates.extend(
        [
            "/usr/local/bin/docker",
            "/opt/homebrew/bin/docker",
            "/Applications/Docker.app/Contents/Resources/bin/docker",
            "/usr/bin/docker",
        ]
        .map(PathBuf::from),
    );
    #[cfg(target_os = "linux")]
    candidates.extend(["/usr/bin/docker", "/usr/local/bin/docker", "/snap/bin/docker"].map(PathBuf::from));
    #[cfg(windows)]
    for var in ["ProgramW6432", "ProgramFiles"] {
        if let Some(dir) = std::env::var_os(var) {
            candidates.push(PathBuf::from(dir).join(r"Docker\Docker\resources\bin\docker.exe"));
        }
    }
    #[cfg(unix)]
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".orbstack/bin/docker"));
        candidates.push(home.join(".docker/bin/docker"));
        candidates.push(home.join(".rd/bin/docker"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        let exe = format!("docker{}", std::env::consts::EXE_SUFFIX);
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join(&exe));
        }
    }
    candidates.into_iter().find(|p| p.exists())
}

fn env_map(env: &serde_json::Value) -> HashMap<String, String> {
    env.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

async fn docker_containers() -> Vec<Discovered> {
    let Some(bin) = docker_bin() else {
        return vec![];
    };
    let ps = match timeout(
        Duration::from_secs(4),
        crate::os::command(&bin).args(["ps", "--format", "{{.ID}}"]).output(),
    )
    .await
    {
        Ok(Ok(o)) if o.status.success() => o,
        _ => return vec![],
    };
    let ids: Vec<String> = String::from_utf8_lossy(&ps.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if ids.is_empty() {
        return vec![];
    }
    let mut args = vec!["inspect".to_string()];
    args.extend(ids);
    let inspect = match timeout(Duration::from_secs(5), crate::os::command(&bin).args(&args).output()).await {
        Ok(Ok(o)) if o.status.success() => o,
        _ => return vec![],
    };
    let Ok(list) = serde_json::from_slice::<Vec<serde_json::Value>>(&inspect.stdout) else {
        return vec![];
    };
    let mut out = Vec::new();
    for c in list {
        let image = c["Config"]["Image"].as_str().unwrap_or("").to_string();
        let name = c["Name"].as_str().unwrap_or("").trim_start_matches('/').to_string();
        let env = env_map(&c["Config"]["Env"]);
        let img = image.to_lowercase();
        // Container command line, for `redis-server --requirepass …`.
        let args: Vec<String> = c["Config"]["Cmd"]
            .as_array()
            .into_iter()
            .chain(c["Args"].as_array())
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let Some(engine) = container_engine(&img, &env) else { continue };
        let internal = match engine {
            Engine::Postgres => "5432/tcp",
            Engine::Mysql => "3306/tcp",
            Engine::Mssql => "1433/tcp",
            Engine::Redis => "6379/tcp",
            Engine::Mongodb => "27017/tcp",
            _ => continue,
        };
        let host_port = c["NetworkSettings"]["Ports"][internal]
            .as_array()
            .and_then(|a| a.iter().find_map(|b| b["HostPort"].as_str()))
            .and_then(|p| p.parse::<u16>().ok());
        let Some(port) = host_port else { continue };
        if matches!(engine, Engine::Mssql | Engine::Redis | Engine::Mongodb) {
            let (user, pass, db, flavor) = match engine {
                Engine::Mssql => (
                    "sa".to_string(),
                    env.get("MSSQL_SA_PASSWORD").or_else(|| env.get("SA_PASSWORD")).cloned(),
                    String::new(),
                    if img.contains("azure-sql-edge") { "Azure SQL Edge" } else { "SQL Server" },
                ),
                Engine::Redis => {
                    let pass = env
                        .get("REDIS_PASSWORD")
                        .or_else(|| env.get("VALKEY_PASSWORD"))
                        .cloned()
                        .or_else(|| requirepass(&args));
                    let flavor = if img.contains("valkey") {
                        "Valkey"
                    } else if img.contains("keydb") {
                        "KeyDB"
                    } else if img.contains("dragonfly") {
                        "Dragonfly"
                    } else {
                        "Redis"
                    };
                    (String::new(), pass, String::new(), flavor)
                }
                _ => {
                    let user = env
                        .get("MONGO_INITDB_ROOT_USERNAME")
                        .or_else(|| env.get("MONGODB_ROOT_USER"))
                        .or_else(|| env.get("MONGODB_USERNAME"))
                        .cloned()
                        .unwrap_or_default();
                    let pass = env
                        .get("MONGO_INITDB_ROOT_PASSWORD")
                        .or_else(|| env.get("MONGODB_ROOT_PASSWORD"))
                        .or_else(|| env.get("MONGODB_PASSWORD"))
                        .cloned();
                    let db = env.get("MONGO_INITDB_DATABASE").or_else(|| env.get("MONGODB_DATABASE")).cloned().unwrap_or_default();
                    (user, pass, db, "MongoDB")
                }
            };
            out.push(Discovered {
                id: format!("docker:{name}"),
                engine,
                flavor: flavor.into(),
                version: None,
                host: "127.0.0.1".into(),
                port,
                source: "Docker".into(),
                container: Some(name),
                image: Some(image),
                username: Some(user),
                password: pass,
                database: Some(db),
                auth_ok: None,
                auth_error: None,
                already_saved: false,
                note: None,
            });
            continue;
        }
        let (user, pass, db) = if engine == Engine::Postgres {
            let user = env.get("POSTGRES_USER").cloned().unwrap_or_else(|| "postgres".into());
            let db = env.get("POSTGRES_DB").cloned().unwrap_or_else(|| user.clone());
            (user, env.get("POSTGRES_PASSWORD").cloned(), db)
        } else {
            let get = |k: &str| env.get(&format!("MYSQL_{k}")).or_else(|| env.get(&format!("MARIADB_{k}"))).cloned();
            let root_pw = get("ROOT_PASSWORD");
            let (user, pass) = if let Some(pw) = root_pw {
                ("root".to_string(), Some(pw))
            } else if let (Some(u), Some(p)) = (get("USER"), get("PASSWORD")) {
                (u, Some(p))
            } else {
                ("root".to_string(), None)
            };
            (user, pass, get("DATABASE").unwrap_or_default())
        };
        let maria = img.contains("mariadb");
        out.push(Discovered {
            id: format!("docker:{name}"),
            engine,
            flavor: match engine {
                Engine::Postgres => "PostgreSQL".into(),
                _ if maria => "MariaDB".into(),
                _ => "MySQL".into(),
            },
            version: None,
            host: "127.0.0.1".into(),
            port,
            source: "Docker".into(),
            container: Some(name),
            image: Some(image),
            username: Some(user),
            password: pass,
            database: Some(db),
            auth_ok: None,
            auth_error: None,
            already_saved: false,
            note: None,
        });
    }
    out
}

/// Which database a container runs, from its image name and environment.
fn container_engine(img: &str, env: &HashMap<String, String>) -> Option<Engine> {
    // Admin UIs that ship next to databases (and whose names contain them).
    if ["mongo-express", "redis-commander", "redisinsight", "pgadmin", "phpmyadmin", "adminer", "redis-exporter", "mongodb-exporter", "postgres-exporter", "mysqld-exporter"]
        .iter()
        .any(|ui| img.contains(ui))
    {
        return None;
    }
    if img.contains("postgres") || img.contains("postgis") || img.contains("timescale")
        || env.contains_key("POSTGRES_PASSWORD") || env.contains_key("POSTGRES_USER")
    {
        Some(Engine::Postgres)
    } else if img.contains("mysql") || img.contains("mariadb") || img.contains("percona")
        || env.contains_key("MYSQL_ROOT_PASSWORD") || env.contains_key("MARIADB_ROOT_PASSWORD")
        || env.contains_key("MYSQL_ALLOW_EMPTY_PASSWORD")
    {
        Some(Engine::Mysql)
    } else if img.contains("mssql") || img.contains("azure-sql-edge") || env.contains_key("MSSQL_SA_PASSWORD") {
        Some(Engine::Mssql)
    } else if img.contains("mongo") || env.contains_key("MONGO_INITDB_ROOT_USERNAME") {
        Some(Engine::Mongodb)
    } else if img.contains("redis") || img.contains("valkey") || img.contains("keydb") || img.contains("dragonfly") {
        Some(Engine::Redis)
    } else {
        None
    }
}

/// `--requirepass <pw>` from a redis-server command line.
fn requirepass(args: &[String]) -> Option<String> {
    let mut it = args.iter().flat_map(|a| a.split_whitespace().map(str::to_string).collect::<Vec<_>>());
    while let Some(a) = it.next() {
        if a == "--requirepass" {
            return it.next().map(|p| p.trim_matches(|c| c == '"' || c == '\'').to_string());
        }
        if let Some(p) = a.strip_prefix("--requirepass=") {
            return Some(p.to_string());
        }
    }
    None
}


pub async fn scan() -> Vec<Discovered> {
    let docker_fut = docker_containers();
    let mut probes = Vec::new();
    for &p in PG_PORTS.iter().chain(MY_PORTS).chain(MS_PORTS).chain(REDIS_PORTS).chain(MONGO_PORTS) {
        probes.push(async move { (p, fingerprint(p).await) });
    }
    let (docker, probed) = tokio::join!(docker_fut, futures_util::future::join_all(probes));

    let mut found: Vec<Discovered> = docker;
    for (port, fp) in probed {
        let Some(fp) = fp else { continue };
        if found.iter().any(|d| d.port == port) {
            // Already known from Docker — just enrich the version.
            if let Some(d) = found.iter_mut().find(|d| d.port == port) {
                if d.version.is_none() {
                    d.version = fp.version.clone();
                }
                if d.note.is_none() && !(fp.note.as_deref() == Some("Password required") && d.password.is_some()) {
                    d.note = fp.note.clone();
                }
            }
            continue;
        }
        let source = port_owner(port).await.unwrap_or_else(|| "Local server".into());
        let (user, db) = match fp.engine {
            Engine::Postgres => (os_user(), "postgres".to_string()),
            Engine::Mysql => ("root".to_string(), String::new()),
            Engine::Mssql => ("sa".to_string(), String::new()),
            _ => (String::new(), String::new()),
        };
        found.push(Discovered {
            id: format!("tcp:{port}"),
            engine: fp.engine,
            flavor: fp.flavor,
            version: fp.version,
            host: "127.0.0.1".into(),
            port,
            source,
            container: None,
            image: None,
            username: Some(user),
            password: None,
            database: Some(db),
            auth_ok: None,
            auth_error: None,
            already_saved: false,
            note: fp.note,
        });
    }

    // Discovery only identifies what is listening on this computer. It never
    // tries to log in: a connection is attempted only when the user clicks
    // Connect (a local port may be a tunnel to a server they don't control).
    found.sort_by_key(|d| (d.engine.as_str().to_string(), d.port));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_container_engines() {
        let none = HashMap::new();
        assert_eq!(container_engine("redis:7-alpine", &none), Some(Engine::Redis));
        assert_eq!(container_engine("valkey/valkey:8", &none), Some(Engine::Redis));
        assert_eq!(container_engine("mongo:7", &none), Some(Engine::Mongodb));
        assert_eq!(container_engine("mongo-express", &none), None);
        assert_eq!(container_engine("redis/redisinsight:latest", &none), None);
        assert_eq!(container_engine("mcr.microsoft.com/mssql/server:2022-latest", &none), Some(Engine::Mssql));
        assert_eq!(container_engine("mcr.microsoft.com/azure-sql-edge", &none), Some(Engine::Mssql));
        assert_eq!(container_engine("postgres:16", &none), Some(Engine::Postgres));
        assert_eq!(container_engine("nginx", &none), None);
    }

    #[test]
    fn finds_requirepass() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(requirepass(&a(&["redis-server", "--requirepass", "s3cret"])), Some("s3cret".into()));
        assert_eq!(requirepass(&a(&["sh", "-c", "redis-server --appendonly yes --requirepass 'pw'"])), Some("pw".into()));
        assert_eq!(requirepass(&a(&["redis-server", "--requirepass=x"])), Some("x".into()));
        assert_eq!(requirepass(&a(&["redis-server"])), None);
    }

    #[test]
    fn parses_prelogin_reply() {
        // Reply header + VERSION (16.0.4135) + ENCRYPTION + terminator.
        let mut r = vec![0x04, 0x01, 0, 0, 0, 0, 1, 0];
        r.extend_from_slice(&[0x00, 0x00, 0x0B, 0x00, 0x06, 0x01, 0x00, 0x11, 0x00, 0x01, 0xFF]);
        r.extend_from_slice(&[16, 0, 0x10, 0x27, 0, 0, 0x00]);
        assert_eq!(parse_prelogin(&r).as_deref(), Some("16.0.4135"));
        assert_eq!(mssql_release("16.0.4135"), Some("2022"));
        assert!(parse_prelogin(b"HTTP/1.1 400").is_none());
        assert_eq!(prelogin_packet()[0], 0x12);
    }

    #[test]
    fn builds_mongo_hello() {
        let m = mongo_build_info_msg();
        assert_eq!(i32::from_le_bytes(m[0..4].try_into().unwrap()) as usize, m.len());
        assert_eq!(i32::from_le_bytes(m[12..16].try_into().unwrap()), 2013);
        // A reply is parsed the same way.
        let doc = parse_op_msg(&m).unwrap();
        assert_eq!(doc.get_i32("buildInfo").unwrap(), 1);
    }

    #[test]
    fn labels_port_owners() {
        assert_eq!(owner_label("/Applications/Postgres.app/Contents/Versions/16/bin/postgres"), "Postgres.app");
        assert_eq!(owner_label("/opt/homebrew/Cellar/postgresql@16/16.4/bin/postgres"), "Homebrew");
        assert_eq!(owner_label("/home/linuxbrew/.linuxbrew/Cellar/mysql/9.1.0/bin/mysqld"), "Homebrew");
        assert_eq!(owner_label(r"C:\Program Files\Docker\Docker\resources\com.docker.backend.exe"), "Docker");
        assert_eq!(owner_label(r"C:\laragon\bin\mysql\mysql-8.0.30-winx64\bin\mysqld.exe"), "Laragon");
        assert_eq!(owner_label(r"C:\xampp\mysql\bin\mysqld.exe"), "XAMPP");
        assert_eq!(owner_label(r"C:\Windows\System32\wslrelay.exe"), "WSL");
        assert_eq!(owner_label("/usr/lib/postgresql/16/bin/postgres"), "Local server");
        assert_eq!(owner_label(r"C:\Program Files\PostgreSQL\16\bin\postgres.exe"), "Local server");
    }
}
