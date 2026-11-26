pub mod duckdb;
pub mod mongo;
pub mod mssql;
pub mod mysql;
pub mod postgres;
pub mod redis;
pub mod sqlite;
pub mod tls;

use crate::error::{AppError, AppResult};
use crate::model::*;
use crate::ssh::{self, SshParams, Tunnel};
use tokio::sync::{Mutex, RwLock};

pub enum Driver {
    Pg(postgres::PgSession),
    My(mysql::MySession),
    Lite(sqlite::LiteSession),
    Duck(duckdb::DuckSession),
    Ms(mssql::MsSession),
    Redis(redis::RedisSession),
    Mongo(mongo::MongoSession),
}

/// One live connection to one database. Each session holds two physical
/// connections: `editor` (user SQL, keeps session state like transactions)
/// and `aux` (sidebar metadata, table browsing and staged edits) so a slow
/// query in the editor never freezes the rest of the UI.
///
/// If the server drops the connection (restart, network blip, idle timeout)
/// the session reconnects transparently. Reads are retried once; writes are
/// never retried automatically.
pub struct Session {
    pub id: String,
    pub connection: ConnectionConfig,
    pub info: ServerInfo,
    driver: RwLock<Driver>,
    password: Option<String>,
    database: Option<String>,
    current_schema: Mutex<Option<String>>,
    reconnect_lock: Mutex<()>,
    /// SSH tunnel settings (with the trusted host key) and the live tunnel.
    ssh: Option<SshParams>,
    tunnel: Mutex<Option<Tunnel>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Editor,
    Aux,
}

/// Staged edits target exactly one row each (by primary key). Anything else
/// means the row changed underneath us, so the whole transaction is aborted.
pub fn check_one(expect_one: bool, r: QueryResult) -> AppResult<QueryResult> {
    if !expect_one {
        return Ok(r);
    }
    match r.rows_affected {
        Some(1) => Ok(r),
        Some(0) => Err(AppError::new(
            "db",
            "This row no longer matches what was loaded (it was changed or deleted elsewhere). Nothing was committed — reload the table and try again.",
        )
        .with_code("row_not_found")),
        Some(n) => Err(AppError::new(
            "db",
            format!("This change would have affected {n} rows instead of 1. Nothing was committed."),
        )
        .with_code("row_count_mismatch")),
        None => Err(AppError::new("db", "The database did not report how many rows changed. Nothing was committed.")),
    }
}

fn not_sql(engine: Engine) -> AppError {
    AppError::invalid(format!("{} connections don't support SQL", engine_label(engine)))
}

pub fn engine_label(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "PostgreSQL",
        Engine::Mysql => "MySQL",
        Engine::Sqlite => "SQLite",
        Engine::Mssql => "SQL Server",
        Engine::Duckdb => "DuckDB",
        Engine::Redis => "Redis",
        Engine::Mongodb => "MongoDB",
    }
}

/// The host and port the driver should dial: the server itself, or the local
/// end of the SSH tunnel.
fn endpoint(cfg: &ConnectionConfig, tunnel: Option<&Tunnel>) -> ConnectionConfig {
    let mut c = cfg.clone();
    if c.port == 0 {
        c.port = c.engine.default_port();
    }
    if let Some(t) = tunnel {
        c.host = "127.0.0.1".into();
        c.port = t.local_port;
    }
    c
}

async fn open_driver(cfg: &ConnectionConfig, password: Option<&str>, db: &str) -> AppResult<(Driver, ServerInfo)> {
    Ok(match cfg.engine {
        Engine::Postgres => {
            let (s, i) = Box::pin(postgres::PgSession::open(cfg, password, db)).await?;
            (Driver::Pg(s), i)
        }
        Engine::Mysql => {
            let (s, i) = Box::pin(mysql::MySession::open(cfg, password, db)).await?;
            (Driver::My(s), i)
        }
        Engine::Sqlite => {
            let cfg2 = cfg.clone();
            let (s, i) = tokio::task::spawn_blocking(move || sqlite::LiteSession::open(&cfg2)).await??;
            (Driver::Lite(s), i)
        }
        Engine::Duckdb => {
            let cfg2 = cfg.clone();
            let (s, i) = tokio::task::spawn_blocking(move || duckdb::DuckSession::open(&cfg2)).await??;
            (Driver::Duck(s), i)
        }
        Engine::Mssql => {
            let host = if cfg.host.trim().is_empty() { "localhost" } else { cfg.host.trim() };
            let (s, i) = Box::pin(mssql::MsSession::open(cfg, password, db, host, cfg.port)).await?;
            (Driver::Ms(s), i)
        }
        Engine::Redis => {
            let (s, i) = Box::pin(redis::RedisSession::open(cfg, password, db)).await?;
            (Driver::Redis(s), i)
        }
        Engine::Mongodb => {
            let (s, i) = Box::pin(mongo::MongoSession::open(cfg, password, db)).await?;
            (Driver::Mongo(s), i)
        }
    })
}

/// Opens the SSH tunnel for a connection, if it uses one.
async fn open_tunnel(cfg: &ConnectionConfig, ssh: Option<&SshParams>) -> AppResult<Option<Tunnel>> {
    if !cfg.ssh_enabled || cfg.engine.is_file() {
        return Ok(None);
    }
    let Some(p) = ssh else {
        return Err(AppError::invalid("This connection uses an SSH tunnel but its SSH settings are missing"));
    };
    let target = if cfg.host.trim().is_empty() { "127.0.0.1" } else { cfg.host.trim() };
    let port = if cfg.port == 0 { cfg.engine.default_port() } else { cfg.port };
    Ok(Some(Box::pin(ssh::open_tunnel(p, target, port)).await?))
}

const LOST_RETRY_MSG: &str =
    "The connection to the server was lost (was it restarted?). Rowglow reconnected — run the statement again.";

/// Runs `$body` with `$d` bound to the current driver; if the connection was
/// lost, reconnects and (when `$retry`) runs it once more.
macro_rules! with_driver {
    ($self:ident, $retry:expr, |$d:ident| $body:expr) => {{
        let first = {
            let guard = $self.driver.read().await;
            let $d: &Driver = &guard;
            $body
        };
        match first {
            Err(e) if $self.connection_lost(&e).await => {
                $self.reconnect().await?;
                if $retry {
                    let guard = $self.driver.read().await;
                    let $d: &Driver = &guard;
                    $body
                } else {
                    Err(AppError::new("connection", LOST_RETRY_MSG))
                }
            }
            other => other,
        }
    }};
}

impl Session {
    pub async fn open(
        id: String,
        cfg: &ConnectionConfig,
        password: Option<&str>,
        database: Option<&str>,
        ssh: Option<SshParams>,
    ) -> AppResult<Session> {
        let db = database
            .filter(|d| !d.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| cfg.database.clone());
        let tunnel = Box::pin(open_tunnel(cfg, ssh.as_ref())).await?;
        let (driver, mut info) = Box::pin(open_driver(&endpoint(cfg, tunnel.as_ref()), password, &db)).await?;
        info.session_id = id.clone();
        Ok(Session {
            id,
            connection: cfg.clone(),
            info,
            driver: RwLock::new(driver),
            password: password.map(str::to_string),
            database: Some(db),
            current_schema: Mutex::new(None),
            reconnect_lock: Mutex::new(()),
            ssh,
            tunnel: Mutex::new(tunnel),
        })
    }

    pub fn engine(&self) -> Engine {
        self.connection.engine
    }

    /// The Redis connection of this session (cheap clone of a multiplexed handle).
    pub async fn redis(&self) -> AppResult<redis::RedisSession> {
        let first = match &*self.driver.read().await {
            Driver::Redis(r) => r.clone(),
            _ => return Err(AppError::invalid("Not a Redis connection")),
        };
        Ok(first)
    }

    /// Runs a Redis call; if the connection dropped, reconnects once and retries.
    pub async fn with_redis<T, F, Fut>(&self, f: F) -> AppResult<T>
    where
        F: Fn(redis::RedisSession) -> Fut,
        Fut: std::future::Future<Output = AppResult<T>>,
    {
        match f(self.redis().await?).await {
            Err(e) if e.kind == "connection" => {
                self.reconnect().await?;
                f(self.redis().await?).await
            }
            other => other,
        }
    }

    /// The MongoDB client of this session (cheap clone).
    pub async fn mongo(&self) -> AppResult<mongo::MongoSession> {
        match &*self.driver.read().await {
            Driver::Mongo(m) => Ok(m.clone()),
            _ => Err(AppError::invalid("Not a MongoDB connection")),
        }
    }

    /// A second, independent session with the same settings (own connections,
    /// own SSH tunnel) — used by long jobs such as CSV import.
    pub async fn fresh(&self) -> AppResult<Session> {
        Session::open(
            crate::util::new_id(),
            &self.connection,
            self.password.as_deref(),
            self.database.as_deref(),
            self.ssh.clone(),
        )
        .await
    }

    async fn connection_lost(&self, e: &AppError) -> bool {
        if e.kind != "connection" {
            return false;
        }
        match &*self.driver.read().await {
            Driver::Pg(s) => s.is_closed(),
            Driver::My(_) | Driver::Ms(_) | Driver::Redis(_) | Driver::Mongo(_) => true,
            Driver::Lite(_) | Driver::Duck(_) => false,
        }
    }

    async fn reconnect(&self) -> AppResult<()> {
        let _one_at_a_time = self.reconnect_lock.lock().await;
        if let Driver::Pg(s) = &*self.driver.read().await {
            if !s.is_closed() {
                return Ok(()); // another task already reconnected
            }
        }
        let db = self.database.clone().unwrap_or_default();
        let mut tunnel = self.tunnel.lock().await;
        if tunnel.as_ref().is_some_and(|t| !t.is_alive()) || (tunnel.is_none() && self.connection.ssh_enabled) {
            *tunnel = None;
            *tunnel = Box::pin(open_tunnel(&self.connection, self.ssh.as_ref())).await?;
        }
        let cfg = endpoint(&self.connection, tunnel.as_ref());
        drop(tunnel);
        let (driver, _) = Box::pin(open_driver(&cfg, self.password.as_deref(), &db)).await?;
        *self.driver.write().await = driver;
        if let Some(schema) = self.current_schema.lock().await.clone() {
            let _ = self.use_schema_inner(&schema).await;
        }
        Ok(())
    }

    pub async fn execute(&self, ch: Channel, sql: &str, limit: usize) -> AppResult<QueryResult> {
        let editor = ch == Channel::Editor;
        // Aux queries are Rowglow's own SELECTs (browsing, counts, exports): safe to retry.
        with_driver!(self, !editor, |d| match d {
            Driver::Pg(s) => {
                if editor {
                    s.editor.run(sql, limit).await
                } else {
                    s.aux.run(sql, limit).await
                }
            }
            Driver::My(s) => s.run(editor, sql, limit).await,
            Driver::Lite(s) => s.run(editor, sql, limit).await,
            Driver::Duck(s) => s.run(editor, sql, limit).await,
            Driver::Ms(s) => s.run(editor, sql, limit).await,
            _ => Err(not_sql(self.engine())),
        })
    }

    pub async fn transaction(&self, stmts: &[String], expect_one: bool) -> Result<Vec<QueryResult>, (usize, AppError)> {
        let res = {
            let guard = self.driver.read().await;
            match &*guard {
                Driver::Pg(s) => s.transaction(stmts, expect_one).await,
                Driver::My(s) => s.transaction(stmts, expect_one).await,
                Driver::Lite(s) => s.transaction(stmts, expect_one).await,
                Driver::Duck(s) => s.transaction(stmts, expect_one).await,
                Driver::Ms(s) => s.transaction(stmts, expect_one).await,
                _ => Err((0, not_sql(self.engine()))),
            }
        };
        match res {
            Err((i, e)) if self.connection_lost(&e).await => {
                let _ = self.reconnect().await;
                Err((
                    i,
                    AppError::new(
                        "connection",
                        "The connection was lost while saving. Rowglow reconnected — reload the table to check whether the changes were applied before trying again.",
                    ),
                ))
            }
            other => other,
        }
    }

    pub async fn cancel(&self) -> AppResult<()> {
        match &*self.driver.read().await {
            Driver::Pg(s) => s.cancel().await,
            Driver::My(s) => s.cancel().await,
            Driver::Lite(s) => {
                s.cancel();
                Ok(())
            }
            Driver::Duck(s) => {
                s.cancel();
                Ok(())
            }
            Driver::Ms(s) => s.cancel().await,
            _ => Ok(()),
        }
    }

    async fn use_schema_inner(&self, schema: &str) -> AppResult<()> {
        match &*self.driver.read().await {
            Driver::Pg(s) => s.use_schema(schema).await,
            Driver::My(s) => s.use_schema(schema).await,
            Driver::Duck(s) => duck_use_schema(s, schema).await,
            _ => Ok(()),
        }
    }

    pub async fn use_schema(&self, schema: &str) -> AppResult<()> {
        *self.current_schema.lock().await = Some(schema.to_string());
        with_driver!(self, true, |d| match d {
            Driver::Pg(s) => s.use_schema(schema).await,
            Driver::My(s) => s.use_schema(schema).await,
            Driver::Duck(s) => duck_use_schema(s, schema).await,
            _ => Ok(()),
        })
    }

    pub async fn databases(&self) -> AppResult<Vec<String>> {
        let fallback = self.info.database.clone();
        with_driver!(self, true, |d| match d {
            Driver::Pg(s) => s.databases().await,
            Driver::My(s) => s.databases().await,
            Driver::Ms(s) => s.databases().await,
            Driver::Redis(s) => s.databases().await,
            Driver::Mongo(s) => s.databases().await,
            Driver::Lite(_) | Driver::Duck(_) => Ok(vec![fallback.clone()]),
        })
    }

    pub async fn schemas(&self) -> AppResult<Vec<String>> {
        with_driver!(self, true, |d| match d {
            Driver::Pg(s) => s.schemas().await,
            Driver::My(s) => s.schemas().await,
            Driver::Lite(s) => s.schemas().await,
            Driver::Duck(s) => s.schemas().await,
            Driver::Ms(s) => s.schemas().await,
            _ => Err(not_sql(self.engine())),
        })
    }

    pub async fn objects(&self, schema: &str) -> AppResult<Vec<DbObject>> {
        with_driver!(self, true, |d| match d {
            Driver::Pg(s) => s.objects(schema).await,
            Driver::My(s) => s.objects(schema).await,
            Driver::Lite(s) => s.objects(schema).await,
            Driver::Duck(s) => s.objects(schema).await,
            Driver::Ms(s) => s.objects(schema).await,
            _ => Err(not_sql(self.engine())),
        })
    }

    pub async fn structure(&self, schema: &str, table: &str) -> AppResult<TableStructure> {
        with_driver!(self, true, |d| match d {
            Driver::Pg(s) => s.structure(schema, table).await,
            Driver::My(s) => s.structure(schema, table).await,
            Driver::Lite(s) => s.structure(schema, table).await,
            Driver::Duck(s) => s.structure(schema, table).await,
            Driver::Ms(s) => s.structure(schema, table).await,
            _ => Err(not_sql(self.engine())),
        })
    }

    pub async fn ddl(&self, schema: &str, name: &str, kind: &str, signature: Option<&str>) -> AppResult<String> {
        with_driver!(self, true, |d| match d {
            Driver::Pg(s) => s.ddl(schema, name, kind, signature).await,
            Driver::My(s) => s.ddl(schema, name, kind).await,
            Driver::Lite(s) => s.ddl(schema, name).await,
            Driver::Duck(s) => s.ddl(schema, name).await,
            Driver::Ms(s) => s.ddl(schema, name, kind).await,
            _ => Err(not_sql(self.engine())),
        })
    }

    pub async fn completions(&self, schema: Option<&str>) -> AppResult<Vec<CompletionTable>> {
        with_driver!(self, true, |d| match d {
            Driver::Pg(s) => s.completions().await,
            Driver::My(s) => s.completions(schema).await,
            Driver::Lite(s) => s.completions().await,
            Driver::Duck(s) => s.completions().await,
            Driver::Ms(s) => s.completions().await,
            _ => Err(not_sql(self.engine())),
        })
    }

    pub async fn relations(&self, schema: &str) -> AppResult<SchemaRelations> {
        with_driver!(self, true, |d| match d {
            Driver::Pg(s) => s.relations(schema).await,
            Driver::My(s) => s.relations(schema).await,
            Driver::Lite(s) => s.relations(schema).await,
            Driver::Duck(s) => s.relations(schema).await,
            Driver::Ms(s) => s.relations(schema).await,
            _ => Err(not_sql(self.engine())),
        })
    }
}

async fn duck_use_schema(s: &duckdb::DuckSession, schema: &str) -> AppResult<()> {
    let sql = format!("SET schema = {}", crate::util::quote_literal(Engine::Duckdb, schema));
    s.run(true, &sql, 1).await.map(|_| ())
}
