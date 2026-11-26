use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Postgres,
    Mysql,
    Sqlite,
    Mssql,
    Duckdb,
    Redis,
    Mongodb,
}

impl Engine {
    pub fn as_str(&self) -> &'static str {
        match self {
            Engine::Postgres => "postgres",
            Engine::Mysql => "mysql",
            Engine::Sqlite => "sqlite",
            Engine::Mssql => "mssql",
            Engine::Duckdb => "duckdb",
            Engine::Redis => "redis",
            Engine::Mongodb => "mongodb",
        }
    }
    pub fn parse(s: &str) -> Option<Engine> {
        match s {
            "postgres" | "postgresql" => Some(Engine::Postgres),
            "mysql" | "mariadb" => Some(Engine::Mysql),
            "sqlite" => Some(Engine::Sqlite),
            "mssql" | "sqlserver" => Some(Engine::Mssql),
            "duckdb" => Some(Engine::Duckdb),
            "redis" => Some(Engine::Redis),
            "mongodb" | "mongo" => Some(Engine::Mongodb),
            _ => None,
        }
    }
    /// Engines that speak SQL (tables, grid, editor, structure).
    pub fn is_sql(&self) -> bool {
        !matches!(self, Engine::Redis | Engine::Mongodb)
    }
    /// Engines that open a local file instead of a server.
    pub fn is_file(&self) -> bool {
        matches!(self, Engine::Sqlite | Engine::Duckdb)
    }
    pub fn default_port(&self) -> u16 {
        match self {
            Engine::Postgres => 5432,
            Engine::Mysql => 3306,
            Engine::Mssql => 1433,
            Engine::Redis => 6379,
            Engine::Mongodb => 27017,
            Engine::Sqlite | Engine::Duckdb => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
}

impl SslMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Prefer => "prefer",
            SslMode::Require => "require",
        }
    }
    pub fn parse(s: &str) -> SslMode {
        match s {
            "disable" => SslMode::Disable,
            "require" | "verify-ca" | "verify-full" => SslMode::Require,
            _ => SslMode::Prefer,
        }
    }
}

/// A saved connection. The password is never part of this struct — it lives
/// in the OS keychain (see `secrets.rs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub engine: Engine,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub ssl_mode: SslMode,
    #[serde(default)]
    pub file_path: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub env: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub group_name: String,
    /// SSH tunnel: connect to the database through an SSH server.
    #[serde(default)]
    pub ssh_enabled: bool,
    #[serde(default)]
    pub ssh_host: String,
    #[serde(default)]
    pub ssh_port: u16,
    #[serde(default)]
    pub ssh_user: String,
    /// "password" | "key"
    #[serde(default)]
    pub ssh_auth: String,
    #[serde(default)]
    pub ssh_key_path: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub last_used_at: Option<i64>,
    /// Output only: whether a password is stored for this connection.
    #[serde(default)]
    pub has_password: bool,
    /// Output only: whether an SSH password / key passphrase is stored.
    #[serde(default)]
    pub has_ssh_secret: bool,
    /// Engine-specific settings (sign-in method, Redis Cluster/Sentinel…).
    #[serde(default)]
    pub options: ConnOptions,
    /// Runtime only, never saved or sent to the UI: the Sentinel password,
    /// read from the keychain when the connection opens.
    #[serde(skip)]
    pub sentinel_password: Option<String>,
}

/// Engine-specific connection settings, stored as JSON in `connections.options`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ConnOptions {
    /// SQL Server sign-in: "sql" (default) | "windows" | "integrated" |
    /// "entra-cli" | "entra-sp".
    pub auth: String,
    /// Microsoft Entra tenant (ID or domain).
    pub tenant: String,
    /// Redis: "standalone" (default) | "cluster" | "sentinel".
    pub redis_mode: String,
    /// Redis Cluster: more seed nodes; Sentinel: the Sentinel nodes
    /// (host:port, separated by commas, spaces or new lines).
    pub redis_nodes: String,
    /// Redis Sentinel: the monitored master's name (e.g. "mymaster").
    pub sentinel_master: String,
    /// Redis Sentinel: user name for the Sentinels themselves (ACL).
    pub sentinel_user: String,
    /// Redis Sentinel: connect to a replica instead of the master.
    pub redis_replica: bool,
}

impl ConnOptions {
    pub fn mssql_auth(&self) -> &str {
        match self.auth.as_str() {
            "" => "sql",
            a => a,
        }
    }
    pub fn redis_mode(&self) -> &str {
        match self.redis_mode.as_str() {
            "" => "standalone",
            m => m,
        }
    }
    /// host:port pairs from `redis_nodes`.
    pub fn nodes(&self, default_port: u16) -> Vec<(String, u16)> {
        self.redis_nodes
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| split_host_port(s, default_port))
            .collect()
    }
}

/// "host:port", "[v6]:port" or "host" → (host, port).
pub fn split_host_port(s: &str, default_port: u16) -> (String, u16) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('[') {
        if let Some((h, p)) = rest.split_once(']') {
            let port = p.trim_start_matches(':').parse().unwrap_or(default_port);
            return (h.to_string(), port);
        }
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().unwrap_or(default_port)),
        _ => (s.to_string(), default_port),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    pub type_name: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub columns: Vec<Column>,
    /// Every value is rendered as text exactly like the server's own client would
    /// show it. `None` is SQL NULL.
    pub rows: Vec<Vec<Option<String>>>,
    /// True if the statement produces a result set (even an empty one).
    pub has_rows: bool,
    pub rows_affected: Option<u64>,
    pub truncated: bool,
    pub duration_ms: f64,
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementOutcome {
    pub sql: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<QueryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::error::AppError>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub session_id: String,
    pub connection_id: String,
    pub engine: Engine,
    /// PostgreSQL | MySQL | MariaDB | SQLite | SQL Server | DuckDB | Redis | MongoDB
    pub flavor: String,
    pub version: String,
    pub database: String,
    pub default_schema: String,
    pub user: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbObject {
    pub name: String,
    /// table | view | matview | foreign | sequence | function | procedure | type
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows_estimate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDef {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default_value: Option<String>,
    pub is_primary_key: bool,
    pub comment: Option<String>,
    pub extra: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexDef {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
    pub definition: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyDef {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_update: String,
    pub on_delete: String,
}

/// A foreign key between two tables, used by the ER diagram.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyRelation {
    pub name: String,
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TablePrimaryKey {
    pub table: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SchemaRelations {
    pub foreign_keys: Vec<ForeignKeyRelation>,
    pub primary_keys: Vec<TablePrimaryKey>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintDef {
    pub name: String,
    pub kind: String,
    pub definition: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerDef {
    pub name: String,
    pub definition: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TableStructure {
    pub columns: Vec<ColumnDef>,
    pub primary_key: Vec<String>,
    pub indexes: Vec<IndexDef>,
    pub foreign_keys: Vec<ForeignKeyDef>,
    pub constraints: Vec<ConstraintDef>,
    pub triggers: Vec<TriggerDef>,
    /// Foreign keys in *other* tables that point at this table.
    pub referenced_by: Vec<ForeignKeyDef>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionTable {
    pub schema: String,
    pub name: String,
    pub kind: String,
    pub columns: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: i64,
    pub connection_id: String,
    pub database: String,
    pub sql: String,
    pub duration_ms: f64,
    pub rows: Option<i64>,
    pub error: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub connection_id: Option<String>,
    pub name: String,
    pub sql: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}
