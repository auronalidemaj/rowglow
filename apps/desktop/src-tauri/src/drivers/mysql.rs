//! MySQL / MariaDB driver. Uses the text protocol so values render exactly as
//! the server formats them. In MySQL a "schema" is a database, so the sidebar's
//! schema switcher browses every database on the server without reconnecting.

use crate::error::{AppError, AppResult};
use crate::model::*;
use crate::util::{hex, quote_ident, quote_literal};
use mysql_async::consts::{ColumnFlags, ColumnType};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, OptsBuilder, SslOpts, Value};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub struct MySession {
    editor: Mutex<Conn>,
    aux: Mutex<Conn>,
    opts: Opts,
    editor_id: u32,
}

type Rows = Vec<Vec<Option<String>>>;

fn build_opts(cfg: &ConnectionConfig, password: Option<&str>, database: &str) -> Opts {
    let host = if cfg.host.trim().is_empty() {
        "127.0.0.1".to_string()
    } else {
        cfg.host.trim().to_string()
    };
    let user = if cfg.username.trim().is_empty() {
        "root".to_string()
    } else {
        cfg.username.trim().to_string()
    };
    let mut b = OptsBuilder::default()
        .user(Some(user))
        .pass(password.filter(|p| !p.is_empty()).map(|p| p.to_string()))
        .db_name(if database.is_empty() { None } else { Some(database.to_string()) })
        .prefer_socket(false)
        .wait_timeout(Some(28_800));
    if host.starts_with('/') {
        b = b.socket(Some(host));
    } else {
        b = b
            .ip_or_hostname(host)
            .tcp_port(if cfg.port == 0 { 3306 } else { cfg.port });
    }
    if cfg.ssl_mode == SslMode::Require {
        b = b.ssl_opts(Some(
            SslOpts::default()
                .with_danger_accept_invalid_certs(true)
                .with_danger_skip_domain_validation(true),
        ));
    }
    Opts::from(b)
}

async fn connect(opts: &Opts) -> AppResult<Conn> {
    tokio::time::timeout(Duration::from_secs(15), Conn::new(opts.clone()))
        .await
        .map_err(|_| AppError::new("connection", "Timed out while connecting to the server"))?
        .map_err(AppError::from)
}

fn is_binary(col: &mysql_async::Column) -> bool {
    col.character_set() == 63
        && matches!(
            col.column_type(),
            ColumnType::MYSQL_TYPE_BLOB
                | ColumnType::MYSQL_TYPE_TINY_BLOB
                | ColumnType::MYSQL_TYPE_MEDIUM_BLOB
                | ColumnType::MYSQL_TYPE_LONG_BLOB
                | ColumnType::MYSQL_TYPE_VAR_STRING
                | ColumnType::MYSQL_TYPE_STRING
                | ColumnType::MYSQL_TYPE_VARCHAR
                | ColumnType::MYSQL_TYPE_GEOMETRY
        )
}

fn type_name(col: &mysql_async::Column) -> String {
    let bin = is_binary(col);
    let flags = col.flags();
    if flags.contains(ColumnFlags::ENUM_FLAG) {
        return "enum".into();
    }
    if flags.contains(ColumnFlags::SET_FLAG) {
        return "set".into();
    }
    let unsigned = flags.contains(ColumnFlags::UNSIGNED_FLAG);
    let base = match col.column_type() {
        ColumnType::MYSQL_TYPE_DECIMAL | ColumnType::MYSQL_TYPE_NEWDECIMAL => "decimal",
        ColumnType::MYSQL_TYPE_TINY => "tinyint",
        ColumnType::MYSQL_TYPE_SHORT => "smallint",
        ColumnType::MYSQL_TYPE_INT24 => "mediumint",
        ColumnType::MYSQL_TYPE_LONG => "int",
        ColumnType::MYSQL_TYPE_LONGLONG => "bigint",
        ColumnType::MYSQL_TYPE_FLOAT => "float",
        ColumnType::MYSQL_TYPE_DOUBLE => "double",
        ColumnType::MYSQL_TYPE_NULL => "null",
        ColumnType::MYSQL_TYPE_TIMESTAMP | ColumnType::MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        ColumnType::MYSQL_TYPE_DATE | ColumnType::MYSQL_TYPE_NEWDATE => "date",
        ColumnType::MYSQL_TYPE_TIME | ColumnType::MYSQL_TYPE_TIME2 => "time",
        ColumnType::MYSQL_TYPE_DATETIME | ColumnType::MYSQL_TYPE_DATETIME2 => "datetime",
        ColumnType::MYSQL_TYPE_YEAR => "year",
        ColumnType::MYSQL_TYPE_BIT => "bit",
        ColumnType::MYSQL_TYPE_JSON => "json",
        ColumnType::MYSQL_TYPE_ENUM => "enum",
        ColumnType::MYSQL_TYPE_SET => "set",
        ColumnType::MYSQL_TYPE_GEOMETRY => "geometry",
        ColumnType::MYSQL_TYPE_VARCHAR | ColumnType::MYSQL_TYPE_VAR_STRING => {
            if bin {
                "varbinary"
            } else {
                "varchar"
            }
        }
        ColumnType::MYSQL_TYPE_STRING => {
            if bin {
                "binary"
            } else {
                "char"
            }
        }
        ColumnType::MYSQL_TYPE_BLOB
        | ColumnType::MYSQL_TYPE_TINY_BLOB
        | ColumnType::MYSQL_TYPE_MEDIUM_BLOB
        | ColumnType::MYSQL_TYPE_LONG_BLOB => {
            if bin {
                "blob"
            } else {
                "text"
            }
        }
        _ => "unknown",
    };
    if unsigned && base.ends_with("int") {
        format!("{base} unsigned")
    } else {
        base.to_string()
    }
}

fn render(v: &Value, col: &mysql_async::Column) -> Option<String> {
    match v {
        Value::NULL => None,
        Value::Bytes(b) => {
            if col.column_type() == ColumnType::MYSQL_TYPE_BIT {
                let mut n: u64 = 0;
                for byte in b.iter().take(8) {
                    n = (n << 8) | *byte as u64;
                }
                Some(n.to_string())
            } else if is_binary(col) {
                Some(format!("0x{}", hex(b).to_uppercase()))
            } else {
                Some(String::from_utf8_lossy(b).into_owned())
            }
        }
        Value::Int(i) => Some(i.to_string()),
        Value::UInt(u) => Some(u.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Double(f) => Some(f.to_string()),
        Value::Date(y, m, d, h, mi, s, us) => Some(if *h == 0 && *mi == 0 && *s == 0 && *us == 0 {
            format!("{y:04}-{m:02}-{d:02}")
        } else if *us == 0 {
            format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
        } else {
            format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}")
        }),
        Value::Time(neg, d, h, mi, s, us) => {
            let hours = *d as u64 * 24 + *h as u64;
            let sign = if *neg { "-" } else { "" };
            Some(if *us == 0 {
                format!("{sign}{hours:02}:{mi:02}:{s:02}")
            } else {
                format!("{sign}{hours:02}:{mi:02}:{s:02}.{us:06}")
            })
        }
    }
}

async fn run_on(conn: &mut Conn, sql: &str, limit: usize) -> AppResult<QueryResult> {
    let start = Instant::now();
    let mut qr = conn.query_iter(sql).await?;
    let mut out = QueryResult::default();
    let cols = qr.columns();
    if let Some(cols) = cols.filter(|c| !c.is_empty()) {
        out.has_rows = true;
        out.columns = cols
            .iter()
            .map(|c| Column {
                name: c.name_str().to_string(),
                type_name: type_name(c),
            })
            .collect();
        while let Some(row) = qr.next().await? {
            if out.rows.len() < limit {
                let vals = (0..row.len())
                    .map(|i| match row.as_ref(i) {
                        Some(v) => render(v, &cols[i]),
                        None => None,
                    })
                    .collect();
                out.rows.push(vals);
            } else {
                out.truncated = true;
            }
        }
        out.rows_affected = Some(out.rows.len() as u64);
    } else {
        out.rows_affected = Some(qr.affected_rows());
    }
    qr.drop_result().await?;
    let warnings = conn.get_warnings();
    if warnings > 0 {
        if let Ok(w) = conn
            .query_iter("SHOW WARNINGS LIMIT 50")
            .await
        {
            if let Ok(rows) = w.collect_and_drop::<mysql_async::Row>().await {
                for r in rows {
                    let level: Option<String> = r.get(0);
                    let msg: Option<String> = r.get(2);
                    out.notices
                        .push(format!("{}: {}", level.unwrap_or_default(), msg.unwrap_or_default()));
                }
            }
        }
    }
    out.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(out)
}

async fn rows_on(conn: &mut Conn, sql: &str) -> AppResult<Rows> {
    let r = run_on(conn, sql, usize::MAX).await?;
    Ok(r.rows)
}

fn s(v: &[Option<String>], i: usize) -> String {
    v.get(i).cloned().flatten().unwrap_or_default()
}
fn o(v: &[Option<String>], i: usize) -> Option<String> {
    v.get(i).cloned().flatten()
}
fn lit(v: &str) -> String {
    quote_literal(Engine::Mysql, v)
}

impl MySession {
    pub async fn open(
        cfg: &ConnectionConfig,
        password: Option<&str>,
        database: &str,
    ) -> AppResult<(MySession, ServerInfo)> {
        let opts = build_opts(cfg, password, database);
        let mut editor = connect(&opts).await?;
        // The aux connection runs SQL Rowglow generates. It reports *matched*
        // rows (CLIENT_FOUND_ROWS) so staged edits can verify they hit exactly
        // one row, and always treats backslashes as escapes, matching our quoting.
        let aux_opts = Opts::from(OptsBuilder::from_opts(opts.clone()).client_found_rows(true));
        let mut aux = connect(&aux_opts).await?;
        let _ = aux
            .query_drop("SET SESSION sql_mode = REPLACE(@@SESSION.sql_mode, 'NO_BACKSLASH_ESCAPES', '')")
            .await;
        if cfg.read_only {
            editor.query_drop("SET SESSION TRANSACTION READ ONLY").await?;
            aux.query_drop("SET SESSION TRANSACTION READ ONLY").await?;
        }
        let r = rows_on(&mut aux, "SELECT VERSION(), CURRENT_USER(), DATABASE()").await?;
        let row = r.first().cloned().unwrap_or_default();
        let version = s(&row, 0);
        let flavor = if version.to_lowercase().contains("mariadb") {
            "MariaDB"
        } else {
            "MySQL"
        };
        let clean_version = version
            .trim_start_matches("5.5.5-")
            .split('-')
            .next()
            .unwrap_or("")
            .to_string();
        let db = o(&row, 2).unwrap_or_default();
        let editor_id = editor.id();
        let info = ServerInfo {
            session_id: String::new(),
            connection_id: cfg.id.clone(),
            engine: Engine::Mysql,
            flavor: flavor.into(),
            version: clean_version,
            user: s(&row, 1),
            default_schema: db.clone(),
            database: db,
            read_only: cfg.read_only,
        };
        Ok((
            MySession {
                editor: Mutex::new(editor),
                aux: Mutex::new(aux),
                opts,
                editor_id,
            },
            info,
        ))
    }

    pub async fn run(&self, editor: bool, sql: &str, limit: usize) -> AppResult<QueryResult> {
        if editor {
            let mut c = self.editor.lock().await;
            run_on(&mut c, sql, limit).await
        } else {
            let mut c = self.aux.lock().await;
            run_on(&mut c, sql, limit).await
        }
    }

    async fn rows(&self, sql: &str) -> AppResult<Rows> {
        let mut c = self.aux.lock().await;
        rows_on(&mut c, sql).await
    }

    pub async fn cancel(&self) -> AppResult<()> {
        let mut c = connect(&self.opts).await?;
        c.query_drop(format!("KILL QUERY {}", self.editor_id)).await?;
        let _ = c.disconnect().await;
        Ok(())
    }

    pub async fn close(&self) {
        // Connections are closed when dropped; nothing else to release.
    }

    pub async fn transaction(&self, stmts: &[String], expect_one: bool) -> Result<Vec<QueryResult>, (usize, AppError)> {
        let mut c = self.aux.lock().await;
        c.query_drop("START TRANSACTION").await.map_err(|e| (0, AppError::from(e)))?;
        let mut results = Vec::new();
        for (i, sql) in stmts.iter().enumerate() {
            let res = run_on(&mut c, sql, 1000).await.and_then(|r| super::check_one(expect_one, r));
            match res {
                Ok(r) => results.push(r),
                Err(e) => {
                    let _ = c.query_drop("ROLLBACK").await;
                    return Err((i, e));
                }
            }
        }
        c.query_drop("COMMIT").await.map_err(|e| (stmts.len(), AppError::from(e)))?;
        Ok(results)
    }

    pub async fn use_schema(&self, schema: &str) -> AppResult<()> {
        let sql = format!("USE {}", quote_ident(Engine::Mysql, schema));
        self.editor.lock().await.query_drop(&sql).await?;
        self.aux.lock().await.query_drop(&sql).await?;
        Ok(())
    }

    pub async fn databases(&self) -> AppResult<Vec<String>> {
        Ok(self.rows("SHOW DATABASES").await?.iter().map(|r| s(r, 0)).collect())
    }

    pub async fn schemas(&self) -> AppResult<Vec<String>> {
        let mut dbs = self.databases().await?;
        let system = ["information_schema", "mysql", "performance_schema", "sys"];
        dbs.sort_by_key(|d| (system.contains(&d.as_str()), d.to_lowercase()));
        Ok(dbs)
    }

    pub async fn objects(&self, schema: &str) -> AppResult<Vec<DbObject>> {
        let mut out = Vec::new();
        let tables = self
            .rows(&format!(
                "SELECT TABLE_NAME, TABLE_TYPE, TABLE_ROWS, TABLE_COMMENT FROM information_schema.TABLES
                 WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME",
                lit(schema)
            ))
            .await?;
        for r in tables {
            let kind = if s(&r, 1).contains("VIEW") { "view" } else { "table" };
            out.push(DbObject {
                name: s(&r, 0),
                kind: kind.into(),
                rows_estimate: o(&r, 2).and_then(|v| v.parse().ok()),
                comment: o(&r, 3).filter(|c| !c.is_empty() && c != "VIEW"),
                signature: None,
            });
        }
        let routines = self
            .rows(&format!(
                "SELECT ROUTINE_NAME, ROUTINE_TYPE FROM information_schema.ROUTINES
                 WHERE ROUTINE_SCHEMA = {} ORDER BY ROUTINE_NAME",
                lit(schema)
            ))
            .await
            .unwrap_or_default();
        for r in routines {
            out.push(DbObject {
                name: s(&r, 0),
                kind: if s(&r, 1) == "PROCEDURE" { "procedure" } else { "function" }.into(),
                rows_estimate: None,
                comment: None,
                signature: None,
            });
        }
        Ok(out)
    }

    pub async fn structure(&self, schema: &str, table: &str) -> AppResult<TableStructure> {
        let (ls, lt) = (lit(schema), lit(table));
        let cols = self
            .rows(&format!(
                "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, COLUMN_KEY, COLUMN_COMMENT, EXTRA
                 FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {ls} AND TABLE_NAME = {lt}
                 ORDER BY ORDINAL_POSITION"
            ))
            .await?;
        let stats = self
            .rows(&format!(
                "SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME, INDEX_TYPE FROM information_schema.STATISTICS
                 WHERE TABLE_SCHEMA = {ls} AND TABLE_NAME = {lt} ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX"
            ))
            .await?;
        let mut indexes: Vec<IndexDef> = Vec::new();
        for r in &stats {
            let name = s(r, 0);
            let col = o(r, 2).unwrap_or_else(|| "(expression)".into());
            if let Some(idx) = indexes.iter_mut().find(|i| i.name == name) {
                idx.columns.push(col);
            } else {
                indexes.push(IndexDef {
                    primary: name == "PRIMARY",
                    unique: s(r, 1) == "0",
                    definition: Some(format!("USING {}", s(r, 3))),
                    name,
                    columns: vec![col],
                });
            }
        }
        let primary_key: Vec<String> = indexes
            .iter()
            .find(|i| i.primary)
            .map(|i| i.columns.clone())
            .unwrap_or_default();
        let columns = cols
            .iter()
            .map(|r| ColumnDef {
                name: s(r, 0),
                data_type: s(r, 1),
                nullable: s(r, 2) == "YES",
                default_value: o(r, 3),
                is_primary_key: primary_key.contains(&s(r, 0)),
                comment: o(r, 5).filter(|c| !c.is_empty()),
                extra: o(r, 6).filter(|c| !c.is_empty()),
            })
            .collect();
        let fk_sql = |filter: &str| {
            format!(
                "SELECT k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME,
                        k.REFERENCED_COLUMN_NAME, r.UPDATE_RULE, r.DELETE_RULE, k.TABLE_SCHEMA, k.TABLE_NAME
                 FROM information_schema.KEY_COLUMN_USAGE k
                 JOIN information_schema.REFERENTIAL_CONSTRAINTS r
                   ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME
                  AND r.TABLE_NAME = k.TABLE_NAME
                 WHERE {filter} AND k.REFERENCED_TABLE_NAME IS NOT NULL
                 ORDER BY k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION"
            )
        };
        let mut foreign_keys: Vec<ForeignKeyDef> = Vec::new();
        for r in self
            .rows(&fk_sql(&format!("k.TABLE_SCHEMA = {ls} AND k.TABLE_NAME = {lt}")))
            .await?
        {
            let name = s(&r, 0);
            if let Some(fk) = foreign_keys.iter_mut().find(|f| f.name == name) {
                fk.columns.push(s(&r, 1));
                fk.ref_columns.push(s(&r, 4));
            } else {
                foreign_keys.push(ForeignKeyDef {
                    name,
                    columns: vec![s(&r, 1)],
                    ref_schema: s(&r, 2),
                    ref_table: s(&r, 3),
                    ref_columns: vec![s(&r, 4)],
                    on_update: s(&r, 5),
                    on_delete: s(&r, 6),
                });
            }
        }
        let mut referenced_by: Vec<ForeignKeyDef> = Vec::new();
        for r in self
            .rows(&fk_sql(&format!(
                "k.REFERENCED_TABLE_SCHEMA = {ls} AND k.REFERENCED_TABLE_NAME = {lt}"
            )))
            .await?
        {
            let name = format!("{}.{}", s(&r, 8), s(&r, 0));
            if let Some(fk) = referenced_by.iter_mut().find(|f| f.name == name) {
                fk.columns.push(s(&r, 4));
                fk.ref_columns.push(s(&r, 1));
            } else {
                referenced_by.push(ForeignKeyDef {
                    name,
                    columns: vec![s(&r, 4)],
                    ref_schema: s(&r, 7),
                    ref_table: s(&r, 8),
                    ref_columns: vec![s(&r, 1)],
                    on_update: s(&r, 5),
                    on_delete: s(&r, 6),
                });
            }
        }
        let mut constraints: Vec<ConstraintDef> = self
            .rows(&format!(
                "SELECT CONSTRAINT_NAME, CONSTRAINT_TYPE FROM information_schema.TABLE_CONSTRAINTS
                 WHERE TABLE_SCHEMA = {ls} AND TABLE_NAME = {lt} ORDER BY CONSTRAINT_TYPE, CONSTRAINT_NAME"
            ))
            .await?
            .iter()
            .map(|r| {
                let name = s(r, 0);
                let kind = match s(r, 1).as_str() {
                    "PRIMARY KEY" => "primary",
                    "UNIQUE" => "unique",
                    "FOREIGN KEY" => "foreign",
                    "CHECK" => "check",
                    _ => "other",
                };
                let definition = match kind {
                    "primary" | "unique" => {
                        let cols = indexes
                            .iter()
                            .find(|i| i.name == name)
                            .map(|i| i.columns.join(", "))
                            .unwrap_or_default();
                        format!("{} ({cols})", s(r, 1))
                    }
                    "foreign" => foreign_keys
                        .iter()
                        .find(|f| f.name == name)
                        .map(|f| {
                            format!(
                                "FOREIGN KEY ({}) REFERENCES {}({})",
                                f.columns.join(", "),
                                f.ref_table,
                                f.ref_columns.join(", ")
                            )
                        })
                        .unwrap_or_default(),
                    _ => s(r, 1),
                };
                ConstraintDef {
                    name,
                    kind: kind.into(),
                    definition,
                }
            })
            .collect();
        if let Ok(checks) = self
            .rows(&format!(
                "SELECT cc.CONSTRAINT_NAME, cc.CHECK_CLAUSE FROM information_schema.CHECK_CONSTRAINTS cc
                 JOIN information_schema.TABLE_CONSTRAINTS tc
                   ON tc.CONSTRAINT_SCHEMA = cc.CONSTRAINT_SCHEMA AND tc.CONSTRAINT_NAME = cc.CONSTRAINT_NAME
                 WHERE tc.TABLE_SCHEMA = {ls} AND tc.TABLE_NAME = {lt}"
            ))
            .await
        {
            for r in checks {
                if let Some(c) = constraints.iter_mut().find(|c| c.name == s(&r, 0)) {
                    c.definition = format!("CHECK ({})", s(&r, 1));
                }
            }
        }
        let triggers = self
            .rows(&format!(
                "SELECT TRIGGER_NAME, CONCAT(ACTION_TIMING, ' ', EVENT_MANIPULATION, ' FOR EACH ROW ', ACTION_STATEMENT)
                 FROM information_schema.TRIGGERS WHERE EVENT_OBJECT_SCHEMA = {ls} AND EVENT_OBJECT_TABLE = {lt}"
            ))
            .await
            .unwrap_or_default()
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
        let qn = format!(
            "{}.{}",
            quote_ident(Engine::Mysql, schema),
            quote_ident(Engine::Mysql, name)
        );
        let (sql, col) = match kind {
            "view" => (format!("SHOW CREATE VIEW {qn}"), 1),
            "procedure" => (format!("SHOW CREATE PROCEDURE {qn}"), 2),
            "function" => (format!("SHOW CREATE FUNCTION {qn}"), 2),
            _ => (format!("SHOW CREATE TABLE {qn}"), 1),
        };
        let r = self.rows(&sql).await?;
        let ddl = r.first().map(|r| s(r, col)).unwrap_or_default();
        if ddl.is_empty() {
            return Err(AppError::new(
                "db",
                "The server did not return a definition (missing privileges?)",
            ));
        }
        Ok(format!("{ddl};\n"))
    }

    pub async fn completions(&self, schema: Option<&str>) -> AppResult<Vec<CompletionTable>> {
        let filter = match schema {
            Some(s) if !s.is_empty() => format!("c.TABLE_SCHEMA = {}", lit(s)),
            _ => "c.TABLE_SCHEMA = DATABASE()".to_string(),
        };
        let rows = self
            .rows(&format!(
                "SELECT c.TABLE_SCHEMA, c.TABLE_NAME, t.TABLE_TYPE, c.COLUMN_NAME, c.COLUMN_TYPE
                 FROM information_schema.COLUMNS c
                 JOIN information_schema.TABLES t ON t.TABLE_SCHEMA = c.TABLE_SCHEMA AND t.TABLE_NAME = c.TABLE_NAME
                 WHERE {filter}
                 ORDER BY c.TABLE_SCHEMA, c.TABLE_NAME, c.ORDINAL_POSITION
                 LIMIT 50000"
            ))
            .await?;
        let mut out: Vec<CompletionTable> = Vec::new();
        for r in rows {
            let (sc, tn) = (s(&r, 0), s(&r, 1));
            match out.last_mut() {
                Some(last) if last.schema == sc && last.name == tn => {
                    last.columns.push((s(&r, 3), s(&r, 4)));
                }
                _ => out.push(CompletionTable {
                    schema: sc,
                    name: tn,
                    kind: if s(&r, 2).contains("VIEW") { "view" } else { "table" }.into(),
                    columns: vec![(s(&r, 3), s(&r, 4))],
                }),
            }
        }
        Ok(out)
    }

    pub async fn relations(&self, schema: &str) -> AppResult<SchemaRelations> {
        let rows = self
            .rows(&format!(
                "SELECT k.CONSTRAINT_NAME, k.TABLE_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME,
                        k.REFERENCED_COLUMN_NAME
                 FROM information_schema.KEY_COLUMN_USAGE k
                 JOIN information_schema.TABLE_CONSTRAINTS tc ON tc.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA
                   AND tc.TABLE_NAME = k.TABLE_NAME AND tc.CONSTRAINT_NAME = k.CONSTRAINT_NAME
                 WHERE k.TABLE_SCHEMA = {} AND tc.CONSTRAINT_TYPE IN ('PRIMARY KEY', 'FOREIGN KEY')
                 ORDER BY k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
                lit(schema)
            ))
            .await?;
        let mut out = SchemaRelations::default();
        for r in rows {
            let (name, table) = (s(&r, 0), s(&r, 1));
            if name == "PRIMARY" {
                match out.primary_keys.last_mut() {
                    Some(pk) if pk.table == table => pk.columns.push(s(&r, 2)),
                    _ => out.primary_keys.push(TablePrimaryKey { table, columns: vec![s(&r, 2)] }),
                }
                continue;
            }
            match out.foreign_keys.last_mut() {
                Some(fk) if fk.table == table && fk.name == name => {
                    fk.columns.push(s(&r, 2));
                    fk.ref_columns.push(s(&r, 5));
                }
                _ => out.foreign_keys.push(ForeignKeyRelation {
                    name,
                    schema: schema.to_string(),
                    table,
                    columns: vec![s(&r, 2)],
                    ref_schema: s(&r, 3),
                    ref_table: s(&r, 4),
                    ref_columns: vec![s(&r, 5)],
                }),
            }
        }
        Ok(out)
    }
}
