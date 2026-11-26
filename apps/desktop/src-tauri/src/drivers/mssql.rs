//! Microsoft SQL Server driver (TDS 7.3+ via tiberius, pure Rust, TLS via
//! rustls). Sign-in: SQL Server logins, Microsoft Entra ID tokens (Azure CLI
//! or service principal, see `entra.rs`) and, in the Windows build, Windows
//! accounts (the current Windows user, or another one with its password).
//!
//! Like the other drivers each session holds two connections: `editor` for the
//! user's SQL and `aux` for browsing, metadata and staged edits.
//!
//! Row counts: tiberius' result stream doesn't expose DONE counts, so writes
//! are sent as `<statement>; SELECT @@ROWCOUNT` in one batch and the count is
//! read back from that last result set. @@ROWCOUNT is scoped to the batch, so
//! rows touched by triggers don't inflate it (which matters for the
//! exactly-one-row check on staged edits).

use crate::error::{AppError, AppResult};
use crate::model::*;
use crate::util::{first_keyword, hex, quote_ident};
use chrono::{FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use futures_util::TryStreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tiberius::{AuthMethod, Client, ColumnData, ColumnType, Config, EncryptionLevel, FromSql, QueryItem};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type Tds = Client<Compat<TcpStream>>;
type Rows = Vec<Vec<Option<String>>>;

const ROWCOUNT_COL: &str = "__rowglow_rowcount";

pub struct MsConn {
    config: Config,
    auth: Arc<Auth>,
    client: Mutex<Option<Tds>>,
    cancel: Notify,
}

/// How a connection signs in. Shared by the editor and aux connections so an
/// Entra token is fetched once and refreshed shortly before it expires
/// (reconnects after an hour would otherwise fail with a stale token).
pub struct Auth {
    spec: AuthSpec,
    token: Mutex<Option<crate::entra::Token>>,
}

enum AuthSpec {
    Sql { user: String, password: String },
    #[cfg(windows)]
    Windows { user: String, password: String },
    #[cfg(windows)]
    Integrated,
    EntraCli { tenant: String },
    EntraSp { tenant: String, client_id: String, secret: String },
}

impl Auth {
    pub fn from_cfg(cfg: &ConnectionConfig, password: Option<&str>) -> AppResult<Auth> {
        let user = cfg.username.trim().to_string();
        let password = password.unwrap_or("").to_string();
        let tenant = cfg.options.tenant.trim().to_string();
        let spec = match cfg.options.mssql_auth() {
            "sql" => AuthSpec::Sql { user, password },
            #[cfg(windows)]
            "windows" => {
                if user.is_empty() {
                    return Err(AppError::invalid("Enter the Windows user name, like CORP\\jane"));
                }
                AuthSpec::Windows { user, password }
            }
            #[cfg(windows)]
            "integrated" => AuthSpec::Integrated,
            #[cfg(not(windows))]
            "windows" | "integrated" => {
                return Err(AppError::invalid(
                    "Windows sign-in works in Rowglow for Windows. On this computer, use a SQL Server login or Microsoft Entra ID.",
                ))
            }
            "entra-cli" => AuthSpec::EntraCli { tenant },
            "entra-sp" => AuthSpec::EntraSp { tenant, client_id: user, secret: password },
            other => return Err(AppError::invalid(format!("Unknown SQL Server sign-in method “{other}”"))),
        };
        Ok(Auth { spec, token: Mutex::new(None) })
    }

    pub fn is_entra(&self) -> bool {
        matches!(self.spec, AuthSpec::EntraCli { .. } | AuthSpec::EntraSp { .. })
    }

    async fn entra_token(&self) -> AppResult<String> {
        let mut cached = self.token.lock().await;
        if let Some(t) = cached.as_ref().filter(|t| t.fresh()) {
            return Ok(t.access_token.clone());
        }
        let t = match &self.spec {
            AuthSpec::EntraCli { tenant } => crate::entra::azure_cli(None, Some(tenant.as_str())).await?,
            AuthSpec::EntraSp { tenant, client_id, secret } => {
                crate::entra::service_principal(&crate::entra::authority(), tenant, client_id, secret).await?
            }
            _ => unreachable!("not an Entra sign-in"),
        };
        let token = t.access_token.clone();
        *cached = Some(t);
        Ok(token)
    }

    pub async fn method(&self) -> AppResult<AuthMethod> {
        Ok(match &self.spec {
            AuthSpec::Sql { user, password } => AuthMethod::sql_server(user, password),
            #[cfg(windows)]
            AuthSpec::Windows { user, password } => AuthMethod::windows(user, password),
            #[cfg(windows)]
            AuthSpec::Integrated => AuthMethod::Integrated,
            AuthSpec::EntraCli { .. } | AuthSpec::EntraSp { .. } => AuthMethod::aad_token(self.entra_token().await?),
        })
    }
}

pub struct MsSession {
    pub editor: MsConn,
    pub aux: MsConn,
    lost: AtomicBool,
}

fn base_config(method: &AuthMethod, database: &str, host: &str, port: u16) -> Config {
    let mut c = Config::new();
    c.host(host);
    c.port(port);
    if !database.trim().is_empty() {
        c.database(database.trim());
    }
    c.application_name("Rowglow");
    c.authentication(method.clone());
    c
}

async fn connect_with(config: &Config) -> Result<Tds, tiberius::error::Error> {
    let mut config = config.clone();
    // Azure SQL may redirect the login to another node once.
    for _ in 0..2 {
        let addr = config.get_addr();
        let tcp = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(&addr))
            .await
            .map_err(|_| tiberius::error::Error::Io {
                kind: std::io::ErrorKind::TimedOut,
                message: format!("Timed out connecting to {addr}"),
            })?
            .map_err(|e| tiberius::error::Error::Io {
                kind: e.kind(),
                message: format!("{e} ({addr})"),
            })?;
        tcp.set_nodelay(true).ok();
        match Box::pin(Client::connect(config.clone(), tcp.compat_write())).await {
            Err(tiberius::error::Error::Routing { host, port }) => {
                config.host(&host);
                config.port(port);
            }
            other => return other,
        }
    }
    Err(tiberius::error::Error::Protocol("too many login redirects".into()))
}

/// `prefer` tries an encrypted connection first and falls back to an
/// unencrypted one if the server doesn't support TLS (like libpq's prefer).
/// Certificates aren't verified, matching the Postgres driver.
async fn connect(
    cfg: &ConnectionConfig,
    auth: &Auth,
    database: &str,
    host: &str,
    port: u16,
) -> AppResult<(Tds, Config)> {
    // Checked before asking Entra for a token.
    if cfg.ssl_mode == SslMode::Disable && auth.is_entra() {
        return Err(AppError::invalid(
            "Microsoft Entra sign-in needs an encrypted connection. Set SSL to Prefer or Require.",
        ));
    }
    let method = auth.method().await?;
    let mut config = base_config(&method, database, host, port);
    match cfg.ssl_mode {
        SslMode::Disable => {
            config.encryption(EncryptionLevel::NotSupported);
            Ok((connect_with(&config).await?, config))
        }
        SslMode::Require => {
            config.encryption(EncryptionLevel::Required);
            config.trust_cert();
            Ok((connect_with(&config).await?, config))
        }
        SslMode::Prefer => {
            config.encryption(EncryptionLevel::On);
            config.trust_cert();
            match connect_with(&config).await {
                Ok(c) => Ok((c, config)),
                Err(e) if matches!(e, tiberius::error::Error::Tls(_)) || e.to_string().to_lowercase().contains("encrypt") => {
                    let mut plain = base_config(&method, database, host, port);
                    plain.encryption(EncryptionLevel::NotSupported);
                    Ok((connect_with(&plain).await?, plain))
                }
                Err(e) => Err(e.into()),
            }
        }
    }
}

fn type_name(t: ColumnType) -> &'static str {
    match t {
        ColumnType::Null => "null",
        ColumnType::Bit | ColumnType::Bitn => "bit",
        ColumnType::Int1 => "tinyint",
        ColumnType::Int2 => "smallint",
        ColumnType::Int4 | ColumnType::Intn => "int",
        ColumnType::Int8 => "bigint",
        ColumnType::Float4 => "real",
        ColumnType::Float8 | ColumnType::Floatn => "float",
        ColumnType::Money => "money",
        ColumnType::Money4 => "smallmoney",
        ColumnType::Datetime | ColumnType::Datetimen => "datetime",
        ColumnType::Datetime4 => "smalldatetime",
        ColumnType::Daten => "date",
        ColumnType::Timen => "time",
        ColumnType::Datetime2 => "datetime2",
        ColumnType::DatetimeOffsetn => "datetimeoffset",
        ColumnType::Decimaln => "decimal",
        ColumnType::Numericn => "numeric",
        ColumnType::Guid => "uniqueidentifier",
        ColumnType::BigVarChar => "varchar",
        ColumnType::BigChar => "char",
        ColumnType::NVarchar => "nvarchar",
        ColumnType::NChar => "nchar",
        ColumnType::Text => "text",
        ColumnType::NText => "ntext",
        ColumnType::BigVarBin => "varbinary",
        ColumnType::BigBinary => "binary",
        ColumnType::Image => "image",
        ColumnType::Xml => "xml",
        ColumnType::Udt => "udt",
        ColumnType::SSVariant => "sql_variant",
    }
}

const TS: &str = "%Y-%m-%d %H:%M:%S%.f";

fn render(d: ColumnData<'static>) -> Option<String> {
    match &d {
        ColumnData::U8(v) => v.map(|x| x.to_string()),
        ColumnData::I16(v) => v.map(|x| x.to_string()),
        ColumnData::I32(v) => v.map(|x| x.to_string()),
        ColumnData::I64(v) => v.map(|x| x.to_string()),
        ColumnData::F32(v) => v.map(|x| x.to_string()),
        ColumnData::F64(v) => v.map(|x| x.to_string()),
        ColumnData::Bit(v) => v.map(|b| if b { "1" } else { "0" }.to_string()),
        ColumnData::String(v) => v.as_ref().map(|s| s.to_string()),
        ColumnData::Guid(v) => v.map(|g| g.to_string().to_uppercase()),
        ColumnData::Binary(v) => v.as_ref().map(|b| format!("0x{}", hex(b).to_uppercase())),
        ColumnData::Numeric(v) => v.map(|n| n.to_string()),
        ColumnData::Xml(v) => v.as_ref().map(|x| x.as_ref().clone().into_string()),
        ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => NaiveDateTime::from_sql(&d)
            .ok()
            .flatten()
            .map(|t| t.format(TS).to_string()),
        ColumnData::Date(_) => NaiveDate::from_sql(&d).ok().flatten().map(|t| t.format("%Y-%m-%d").to_string()),
        ColumnData::Time(_) => NaiveTime::from_sql(&d).ok().flatten().map(|t| t.format("%H:%M:%S%.f").to_string()),
        ColumnData::DateTimeOffset(_) => chrono::DateTime::<FixedOffset>::from_sql(&d)
            .ok()
            .flatten()
            .map(|t| t.format("%Y-%m-%d %H:%M:%S%.f %:z").to_string()),
    }
}

/// Statements whose affected-row count we read back with @@ROWCOUNT.
fn counts_rows(sql: &str) -> bool {
    matches!(first_keyword(sql).as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "WITH")
}

struct ResultSet {
    columns: Vec<Column>,
    rows: Rows,
    truncated: bool,
}

async fn run_on(client: &mut Tds, sql: &str, limit: usize) -> AppResult<QueryResult> {
    let start = Instant::now();
    let body = sql.trim().trim_end_matches(';').trim_end();
    let counted = counts_rows(body);
    let batch = if counted {
        format!("{body}\n;SELECT @@ROWCOUNT AS [{ROWCOUNT_COL}];")
    } else {
        body.to_string()
    };
    let mut sets: Vec<ResultSet> = Vec::new();
    {
        let mut stream = client.simple_query(batch).await?;
        while let Some(item) = stream.try_next().await? {
            match item {
                QueryItem::Metadata(meta) => sets.push(ResultSet {
                    columns: meta
                        .columns()
                        .iter()
                        .map(|c| Column {
                            name: c.name().to_string(),
                            type_name: type_name(c.column_type()).into(),
                        })
                        .collect(),
                    rows: Vec::new(),
                    truncated: false,
                }),
                QueryItem::Row(row) => {
                    let Some(set) = sets.last_mut() else { continue };
                    if set.rows.len() >= limit {
                        // Rows past the limit are read and dropped: stopping
                        // early would need an Attention, and tiberius can't
                        // resynchronise after one that races the end of the
                        // response.
                        set.truncated = true;
                        continue;
                    }
                    set.rows.push(row.into_iter().map(render).collect());
                }
            }
        }
    }
    let mut out = QueryResult::default();
    if counted {
        if let Some(last) = sets.last() {
            if last.columns.len() == 1 && last.columns[0].name == ROWCOUNT_COL {
                out.rows_affected = last.rows.first().and_then(|r| r.first().cloned().flatten()).and_then(|v| v.parse().ok());
                sets.pop();
            }
        }
    }
    let extra = sets.len().saturating_sub(1);
    if let Some(first) = sets.into_iter().next() {
        out.has_rows = true;
        out.columns = first.columns;
        out.truncated = first.truncated;
        if out.rows_affected.is_none() {
            out.rows_affected = Some(first.rows.len() as u64);
        }
        out.rows = first.rows;
    }
    if extra > 0 {
        out.notices.push(format!(
            "The batch returned {} result sets; showing the first. Run statements one at a time to see the others.",
            extra + 1
        ));
    }
    out.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(out)
}

fn s(v: &[Option<String>], i: usize) -> String {
    v.get(i).cloned().flatten().unwrap_or_default()
}
fn o(v: &[Option<String>], i: usize) -> Option<String> {
    v.get(i).cloned().flatten()
}

/// N'…' literal for metadata queries (identifiers can't be parameters in
/// every place we need them, e.g. OBJECT_ID).
fn lit(v: &str) -> String {
    format!("N'{}'", v.replace('\'', "''"))
}

fn qname(schema: &str, table: &str) -> String {
    format!("{}.{}", quote_ident(Engine::Mssql, schema), quote_ident(Engine::Mssql, table))
}

fn fk_action(desc: &str) -> String {
    desc.replace('_', " ")
}

impl MsConn {
    fn new(config: Config, auth: Arc<Auth>, client: Tds) -> MsConn {
        MsConn {
            config,
            auth,
            client: Mutex::new(Some(client)),
            cancel: Notify::new(),
        }
    }

    pub async fn run(&self, sql: &str, limit: usize) -> AppResult<QueryResult> {
        let mut guard = self.client.lock().await;
        if guard.is_none() {
            let mut config = self.config.clone();
            config.authentication(self.auth.method().await?);
            *guard = Some(Box::pin(connect_with(&config)).await?);
        }
        let client = guard.as_mut().expect("client present");
        let res = tokio::select! {
            r = Box::pin(run_on(client, sql, limit)) => Some(r),
            _ = self.cancel.notified() => None,
        };
        match res {
            Some(Err(e)) if e.kind == "connection" => {
                *guard = None; // reconnect on next use
                Err(e)
            }
            Some(r) => r,
            None => {
                // The query future was dropped mid-flight: tell the server to
                // stop, then prove the connection is in sync by checking that a
                // sentinel query returns its own value. If the server had
                // already finished, its acknowledgement can't be matched and
                // the connection is replaced instead.
                let healthy = tokio::time::timeout(Duration::from_secs(10), async {
                    client.cancel_query().await.ok()?;
                    let r = run_on(client, "SELECT N'rowglow-sync' AS s", 1).await.ok()?;
                    (r.rows.len() == 1 && r.rows[0].first().cloned().flatten().as_deref() == Some("rowglow-sync")).then_some(())
                })
                .await
                .ok()
                .flatten()
                .is_some();
                if healthy {
                    Err(AppError::new("cancelled", "Query cancelled"))
                } else {
                    *guard = None;
                    Err(AppError::new(
                        "cancelled",
                        "Query cancelled. Rowglow reset the editor connection, so any open transaction was rolled back.",
                    ))
                }
            }
        }
    }

    async fn rows(&self, sql: &str) -> AppResult<Rows> {
        Ok(self.run(sql, usize::MAX).await?.rows)
    }

    async fn simple(&self, sql: &str) -> AppResult<()> {
        self.run(sql, 1).await.map(|_| ())
    }
}

impl MsSession {
    pub async fn open(
        cfg: &ConnectionConfig,
        password: Option<&str>,
        database: &str,
        host: &str,
        port: u16,
    ) -> AppResult<(MsSession, ServerInfo)> {
        let auth = Arc::new(Auth::from_cfg(cfg, password)?);
        let (editor, config) = Box::pin(connect(cfg, &auth, database, host, port)).await?;
        let aux = Box::pin(connect_with(&config)).await?;
        let session = MsSession {
            editor: MsConn::new(config.clone(), auth.clone(), editor),
            aux: MsConn::new(config, auth, aux),
            lost: AtomicBool::new(false),
        };
        let r = session
            .aux
            .rows(
                "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)), CAST(SERVERPROPERTY('Edition') AS nvarchar(128)),
                        DB_NAME(), SCHEMA_NAME(), SUSER_SNAME()",
            )
            .await?;
        let r = r.first().cloned().unwrap_or_default();
        let edition = s(&r, 1);
        let info = ServerInfo {
            session_id: String::new(),
            connection_id: cfg.id.clone(),
            engine: Engine::Mssql,
            flavor: if edition.contains("Azure") { "Azure SQL".into() } else { "SQL Server".into() },
            version: s(&r, 0),
            database: s(&r, 2),
            default_schema: if s(&r, 3).is_empty() { "dbo".into() } else { s(&r, 3) },
            user: s(&r, 4),
            read_only: cfg.read_only,
        };
        Ok((session, info))
    }

    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }

    pub async fn run(&self, editor: bool, sql: &str, limit: usize) -> AppResult<QueryResult> {
        let r = if editor { self.editor.run(sql, limit).await } else { self.aux.run(sql, limit).await };
        if matches!(&r, Err(e) if e.kind == "connection") {
            self.lost.store(true, Ordering::Relaxed);
        }
        r
    }

    pub async fn cancel(&self) -> AppResult<()> {
        self.editor.cancel.notify_waiters();
        Ok(())
    }

    pub async fn transaction(&self, stmts: &[String], expect_one: bool) -> Result<Vec<QueryResult>, (usize, AppError)> {
        let aux = &self.aux;
        aux.simple("SET XACT_ABORT ON; BEGIN TRANSACTION").await.map_err(|e| (0, e))?;
        let mut results = Vec::new();
        for (i, sql) in stmts.iter().enumerate() {
            match aux.run(sql, 1000).await.and_then(|r| super::check_one(expect_one, r)) {
                Ok(r) => results.push(r),
                Err(e) => {
                    let _ = aux.simple("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await;
                    return Err((i, e));
                }
            }
        }
        aux.simple("COMMIT TRANSACTION").await.map_err(|e| (stmts.len(), e))?;
        Ok(results)
    }

    pub async fn databases(&self) -> AppResult<Vec<String>> {
        Ok(self
            .aux
            .rows("SELECT name FROM sys.databases WHERE state = 0 AND HAS_DBACCESS(name) = 1 ORDER BY name")
            .await?
            .iter()
            .map(|r| s(r, 0))
            .collect())
    }

    pub async fn schemas(&self) -> AppResult<Vec<String>> {
        // schema_id >= 16384 are the fixed database-role schemas (db_owner, …).
        Ok(self
            .aux
            .rows(
                "SELECT name FROM sys.schemas WHERE schema_id < 16384 AND name NOT IN ('sys', 'INFORMATION_SCHEMA', 'guest')
                 ORDER BY CASE WHEN name = SCHEMA_NAME() THEN 0 ELSE 1 END, name",
            )
            .await?
            .iter()
            .map(|r| s(r, 0))
            .collect())
    }

    pub async fn objects(&self, schema: &str) -> AppResult<Vec<DbObject>> {
        let rows = self
            .aux
            .rows(&format!(
                "SELECT o.name,
                        CASE o.type WHEN 'U' THEN 'table' WHEN 'V' THEN 'view' WHEN 'P' THEN 'procedure'
                                    WHEN 'SO' THEN 'sequence' ELSE 'function' END,
                        (SELECT SUM(p.rows) FROM sys.partitions p WHERE p.object_id = o.object_id AND p.index_id IN (0, 1)),
                        CAST(ep.value AS nvarchar(4000))
                 FROM sys.objects o
                 JOIN sys.schemas sc ON sc.schema_id = o.schema_id
                 LEFT JOIN sys.extended_properties ep ON ep.class = 1 AND ep.major_id = o.object_id AND ep.minor_id = 0
                   AND ep.name = 'MS_Description'
                 WHERE sc.name = {} AND o.type IN ('U', 'V', 'P', 'FN', 'IF', 'TF', 'SO') AND o.is_ms_shipped = 0
                 ORDER BY o.name",
                lit(schema)
            ))
            .await?;
        Ok(rows
            .iter()
            .map(|r| DbObject {
                name: s(r, 0),
                kind: s(r, 1),
                rows_estimate: o(r, 2).and_then(|v| v.parse().ok()),
                comment: o(r, 3),
                signature: None,
            })
            .collect())
    }

    async fn fk_rows(&self, filter: &str) -> AppResult<Vec<(String, String, String, ForeignKeyDef)>> {
        // (fk name, parent schema, parent table, fk) — grouped per constraint.
        let rows = self
            .aux
            .rows(&format!(
                "SELECT fk.name, ps.name, pt.name, pc.name, rs.name, rt.name, rc.name,
                        fk.update_referential_action_desc, fk.delete_referential_action_desc
                 FROM sys.foreign_keys fk
                 JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id
                 JOIN sys.tables pt ON pt.object_id = fkc.parent_object_id
                 JOIN sys.schemas ps ON ps.schema_id = pt.schema_id
                 JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id
                 JOIN sys.tables rt ON rt.object_id = fkc.referenced_object_id
                 JOIN sys.schemas rs ON rs.schema_id = rt.schema_id
                 JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id
                 WHERE {filter}
                 ORDER BY ps.name, pt.name, fk.name, fkc.constraint_column_id"
            ))
            .await?;
        let mut out: Vec<(String, String, String, ForeignKeyDef)> = Vec::new();
        for r in rows {
            let (name, ps, pt) = (s(&r, 0), s(&r, 1), s(&r, 2));
            match out.last_mut() {
                Some((n, a, b, fk)) if *n == name && *a == ps && *b == pt => {
                    fk.columns.push(s(&r, 3));
                    fk.ref_columns.push(s(&r, 6));
                }
                _ => out.push((
                    name.clone(),
                    ps,
                    pt,
                    ForeignKeyDef {
                        name,
                        columns: vec![s(&r, 3)],
                        ref_schema: s(&r, 4),
                        ref_table: s(&r, 5),
                        ref_columns: vec![s(&r, 6)],
                        on_update: fk_action(&s(&r, 7)),
                        on_delete: fk_action(&s(&r, 8)),
                    },
                )),
            }
        }
        Ok(out)
    }

    pub async fn structure(&self, schema: &str, table: &str) -> AppResult<TableStructure> {
        let obj = lit(&qname(schema, table));
        let cols = self
            .aux
            .rows(&format!(
                "SELECT c.name,
                        TYPE_NAME(c.user_type_id) +
                        CASE WHEN TYPE_NAME(c.user_type_id) IN ('varchar', 'char', 'varbinary', 'binary')
                               THEN '(' + CASE WHEN c.max_length = -1 THEN 'max' ELSE CAST(c.max_length AS varchar(10)) END + ')'
                             WHEN TYPE_NAME(c.user_type_id) IN ('nvarchar', 'nchar')
                               THEN '(' + CASE WHEN c.max_length = -1 THEN 'max' ELSE CAST(c.max_length / 2 AS varchar(10)) END + ')'
                             WHEN TYPE_NAME(c.user_type_id) IN ('decimal', 'numeric')
                               THEN '(' + CAST(c.precision AS varchar(10)) + ',' + CAST(c.scale AS varchar(10)) + ')'
                             WHEN TYPE_NAME(c.user_type_id) IN ('datetime2', 'time', 'datetimeoffset') AND c.scale <> 7
                               THEN '(' + CAST(c.scale AS varchar(10)) + ')'
                             ELSE '' END,
                        c.is_nullable, dc.definition, c.is_identity,
                        CAST(IDENT_SEED({obj}) AS varchar(40)), CAST(IDENT_INCR({obj}) AS varchar(40)),
                        cc.definition, TYPE_NAME(c.user_type_id), CAST(ep.value AS nvarchar(4000))
                 FROM sys.columns c
                 LEFT JOIN sys.default_constraints dc ON dc.object_id = c.default_object_id
                 LEFT JOIN sys.computed_columns cc ON cc.object_id = c.object_id AND cc.column_id = c.column_id
                 LEFT JOIN sys.extended_properties ep ON ep.class = 1 AND ep.major_id = c.object_id
                   AND ep.minor_id = c.column_id AND ep.name = 'MS_Description'
                 WHERE c.object_id = OBJECT_ID({obj})
                 ORDER BY c.column_id"
            ))
            .await?;
        if cols.is_empty() {
            return Err(AppError::not_found(format!("Table {schema}.{table} not found")));
        }
        let idx = self
            .aux
            .rows(&format!(
                "SELECT i.name, i.is_unique, i.is_primary_key, i.type_desc, c.name, i.is_unique_constraint, i.filter_definition
                 FROM sys.indexes i
                 JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id AND ic.is_included_column = 0
                 JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
                 WHERE i.object_id = OBJECT_ID({obj}) AND i.type > 0
                 ORDER BY i.is_primary_key DESC, i.name, ic.key_ordinal"
            ))
            .await?;
        let mut indexes: Vec<IndexDef> = Vec::new();
        let mut unique_constraints: Vec<String> = Vec::new();
        for r in &idx {
            let name = s(r, 0);
            match indexes.last_mut() {
                Some(i) if i.name == name => i.columns.push(s(r, 4)),
                _ => {
                    if s(r, 5) == "1" {
                        unique_constraints.push(name.clone());
                    }
                    let kind = s(r, 3).to_lowercase().replace('_', " ");
                    indexes.push(IndexDef {
                        name,
                        columns: vec![s(r, 4)],
                        unique: s(r, 1) == "1",
                        primary: s(r, 2) == "1",
                        definition: Some(match o(r, 6) {
                            Some(f) => format!("{kind} WHERE {f}"),
                            None => kind,
                        }),
                    });
                }
            }
        }
        let primary_key: Vec<String> = indexes.iter().find(|i| i.primary).map(|i| i.columns.clone()).unwrap_or_default();
        let columns = cols
            .iter()
            .map(|r| {
                let base_type = s(r, 9).to_lowercase();
                let extra = if s(r, 4) == "1" {
                    Some(format!("identity({},{})", s(r, 5), s(r, 6)))
                } else if let Some(def) = o(r, 7) {
                    Some(format!("generated {def}"))
                } else if base_type == "timestamp" || base_type == "rowversion" {
                    Some("generated (rowversion)".into())
                } else {
                    None
                };
                ColumnDef {
                    name: s(r, 0),
                    data_type: s(r, 1).to_lowercase(),
                    nullable: s(r, 2) == "1",
                    default_value: o(r, 3),
                    is_primary_key: primary_key.contains(&s(r, 0)),
                    comment: o(r, 10),
                    extra,
                }
            })
            .collect();
        let foreign_keys = self
            .fk_rows(&format!("fk.parent_object_id = OBJECT_ID({obj})"))
            .await?
            .into_iter()
            .map(|(_, _, _, fk)| fk)
            .collect();
        let referenced_by = self
            .fk_rows(&format!("fk.referenced_object_id = OBJECT_ID({obj})"))
            .await?
            .into_iter()
            .map(|(_, ps, pt, fk)| ForeignKeyDef {
                name: fk.name,
                columns: fk.ref_columns,
                ref_schema: ps,
                ref_table: pt,
                ref_columns: fk.columns,
                on_update: fk.on_update,
                on_delete: fk.on_delete,
            })
            .collect();
        let mut constraints: Vec<ConstraintDef> = indexes
            .iter()
            .filter(|i| i.primary || unique_constraints.contains(&i.name))
            .map(|i| ConstraintDef {
                name: i.name.clone(),
                kind: if i.primary { "primary" } else { "unique" }.into(),
                definition: format!("{} ({})", if i.primary { "PRIMARY KEY" } else { "UNIQUE" }, i.columns.join(", ")),
            })
            .collect();
        for r in self
            .aux
            .rows(&format!(
                "SELECT name, definition FROM sys.check_constraints WHERE parent_object_id = OBJECT_ID({obj}) ORDER BY name"
            ))
            .await?
        {
            constraints.push(ConstraintDef {
                name: s(&r, 0),
                kind: "check".into(),
                definition: format!("CHECK {}", s(&r, 1)),
            });
        }
        let triggers = self
            .aux
            .rows(&format!(
                "SELECT name, OBJECT_DEFINITION(object_id) FROM sys.triggers WHERE parent_id = OBJECT_ID({obj}) ORDER BY name"
            ))
            .await?
            .iter()
            .map(|r| TriggerDef {
                name: s(r, 0),
                definition: s(r, 1),
            })
            .collect();
        Ok(TableStructure {
            columns,
            primary_key,
            indexes,
            foreign_keys,
            constraints,
            triggers,
            referenced_by,
        })
    }

    pub async fn ddl(&self, schema: &str, name: &str, kind: &str) -> AppResult<String> {
        let qn = qname(schema, name);
        if kind != "table" {
            let def = self
                .aux
                .rows(&format!("SELECT OBJECT_DEFINITION(OBJECT_ID({}))", lit(&qn)))
                .await?
                .first()
                .and_then(|r| o(r, 0));
            return match def {
                Some(d) => Ok(format!("{}\n", d.trim())),
                None => Err(AppError::not_found(format!("No definition found for {schema}.{name} (or no permission to view it)"))),
            };
        }
        let st = self.structure(schema, name).await?;
        let mut lines: Vec<String> = Vec::new();
        for c in &st.columns {
            let mut l = format!("    {} ", quote_ident(Engine::Mssql, &c.name));
            match &c.extra {
                Some(e) if e.starts_with("generated ") && !e.contains("rowversion") => {
                    l.push_str(&format!("AS {}", e.trim_start_matches("generated ")));
                }
                _ => {
                    l.push_str(&c.data_type.to_uppercase());
                    if let Some(e) = c.extra.as_deref().filter(|e| e.starts_with("identity")) {
                        l.push_str(&format!(" {}", e.to_uppercase()));
                    }
                    l.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
                    if let Some(d) = &c.default_value {
                        l.push_str(&format!(" DEFAULT {d}"));
                    }
                }
            }
            lines.push(l);
        }
        for con in &st.constraints {
            lines.push(format!("    CONSTRAINT {} {}", quote_ident(Engine::Mssql, &con.name), con.definition));
        }
        for fk in &st.foreign_keys {
            let cols = fk.columns.iter().map(|c| quote_ident(Engine::Mssql, c)).collect::<Vec<_>>().join(", ");
            let refs = fk.ref_columns.iter().map(|c| quote_ident(Engine::Mssql, c)).collect::<Vec<_>>().join(", ");
            let mut l = format!(
                "    CONSTRAINT {} FOREIGN KEY ({cols}) REFERENCES {} ({refs})",
                quote_ident(Engine::Mssql, &fk.name),
                qname(&fk.ref_schema, &fk.ref_table)
            );
            if fk.on_delete != "NO ACTION" {
                l.push_str(&format!(" ON DELETE {}", fk.on_delete));
            }
            if fk.on_update != "NO ACTION" {
                l.push_str(&format!(" ON UPDATE {}", fk.on_update));
            }
            lines.push(l);
        }
        let mut out = format!("CREATE TABLE {qn} (\n{}\n);\n", lines.join(",\n"));
        let constraint_names: Vec<&str> = st.constraints.iter().map(|c| c.name.as_str()).collect();
        for i in st.indexes.iter().filter(|i| !constraint_names.contains(&i.name.as_str())) {
            let cols = i.columns.iter().map(|c| quote_ident(Engine::Mssql, c)).collect::<Vec<_>>().join(", ");
            let kind = i.definition.clone().unwrap_or_default();
            let clustered = if kind.starts_with("clustered") { "CLUSTERED " } else { "" };
            out.push_str(&format!(
                "\nCREATE {}{clustered}INDEX {} ON {qn} ({cols});",
                if i.unique { "UNIQUE " } else { "" },
                quote_ident(Engine::Mssql, &i.name)
            ));
        }
        for t in &st.triggers {
            out.push_str(&format!("\nGO\n{}\n", t.definition.trim()));
        }
        Ok(out.trim_end().to_string() + "\n")
    }

    pub async fn completions(&self) -> AppResult<Vec<CompletionTable>> {
        let rows = self
            .aux
            .rows(
                "SELECT TOP 50000 sc.name, o.name, CASE o.type WHEN 'V' THEN 'view' ELSE 'table' END, c.name, TYPE_NAME(c.user_type_id)
                 FROM sys.objects o
                 JOIN sys.schemas sc ON sc.schema_id = o.schema_id
                 JOIN sys.columns c ON c.object_id = o.object_id
                 WHERE o.type IN ('U', 'V') AND o.is_ms_shipped = 0
                 ORDER BY sc.name, o.name, c.column_id",
            )
            .await?;
        let mut out: Vec<CompletionTable> = Vec::new();
        for r in rows {
            let (schema, name) = (s(&r, 0), s(&r, 1));
            match out.last_mut() {
                Some(t) if t.schema == schema && t.name == name => t.columns.push((s(&r, 3), s(&r, 4))),
                _ => out.push(CompletionTable {
                    schema,
                    name,
                    kind: s(&r, 2),
                    columns: vec![(s(&r, 3), s(&r, 4))],
                }),
            }
        }
        Ok(out)
    }

    pub async fn relations(&self, schema: &str) -> AppResult<SchemaRelations> {
        let mut out = SchemaRelations::default();
        for (_, ps, pt, fk) in self.fk_rows(&format!("ps.name = {}", lit(schema))).await? {
            out.foreign_keys.push(ForeignKeyRelation {
                name: fk.name,
                schema: ps,
                table: pt,
                columns: fk.columns,
                ref_schema: fk.ref_schema,
                ref_table: fk.ref_table,
                ref_columns: fk.ref_columns,
            });
        }
        let rows = self
            .aux
            .rows(&format!(
                "SELECT t.name, c.name
                 FROM sys.indexes i
                 JOIN sys.tables t ON t.object_id = i.object_id
                 JOIN sys.schemas sc ON sc.schema_id = t.schema_id
                 JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
                 JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
                 WHERE i.is_primary_key = 1 AND sc.name = {}
                 ORDER BY t.name, ic.key_ordinal",
                lit(schema)
            ))
            .await?;
        for r in rows {
            let table = s(&r, 0);
            match out.primary_keys.last_mut() {
                Some(pk) if pk.table == table => pk.columns.push(s(&r, 1)),
                _ => out.primary_keys.push(TablePrimaryKey { table, columns: vec![s(&r, 1)] }),
            }
        }
        Ok(out)
    }
}
