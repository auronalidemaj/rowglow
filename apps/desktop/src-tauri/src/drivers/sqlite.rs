//! SQLite driver (bundled SQLite, no system dependency). Two connections per
//! session — one for the editor, one for browsing — each with an interrupt
//! handle so long queries can be cancelled.

use crate::error::{AppError, AppResult};
use crate::model::*;
use crate::util::{hex, quote_ident};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, InterruptHandle, OpenFlags};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct LiteSession {
    editor: Arc<Mutex<Connection>>,
    aux: Arc<Mutex<Connection>>,
    editor_interrupt: InterruptHandle,
}

type Rows = Vec<Vec<Option<String>>>;

pub fn expand_path(p: &str) -> PathBuf {
    let p = p.trim();
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

fn open_conn(path: &PathBuf, read_only: bool) -> AppResult<Connection> {
    if !path.exists() {
        return Err(AppError::new(
            "connection",
            format!("Database file not found: {}", path.display()),
        ));
    }
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX
    };
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    // Validate it's actually a SQLite database (fails fast on random files).
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))?;
    Ok(conn)
}

/// Creates an empty database file (used by "New SQLite database…").
pub fn create_file(path: &str) -> AppResult<()> {
    let p = expand_path(path);
    if p.exists() {
        return Err(AppError::invalid("A file already exists at that path"));
    }
    let conn = Connection::open(&p)?;
    conn.execute_batch("PRAGMA user_version = 0;")?;
    Ok(())
}

fn fmt_real(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

pub fn run_on(conn: &Connection, sql: &str, limit: usize) -> AppResult<QueryResult> {
    let start = Instant::now();
    let mut stmt = conn.prepare(sql)?;
    let mut out = QueryResult::default();
    let col_count = stmt.column_count();
    if col_count == 0 {
        stmt.raw_execute()?;
        out.rows_affected = Some(conn.changes());
    } else {
        let readonly = stmt.readonly();
        let names: Vec<String> = stmt.column_names().map(|s| s.to_string()).collect();
        let decl: Vec<Option<String>> = stmt
            .columns()
            .map(|c| c.decl_type().map(|s| s.to_lowercase()))
            .collect();
        let mut dyn_types: Vec<Option<&'static str>> = vec![None; col_count];
        out.has_rows = true;
        {
            let mut rows = stmt.raw_query();
            while let Some(row) = rows.next()? {
                if out.rows.len() >= limit {
                    out.truncated = true;
                    if readonly {
                        break;
                    }
                    continue;
                }
                let mut vals = Vec::with_capacity(col_count);
                for i in 0..col_count {
                    let v = match row.get_ref(i)? {
                        ValueRef::Null => None,
                        ValueRef::Integer(n) => {
                            dyn_types[i].get_or_insert("integer");
                            Some(n.to_string())
                        }
                        ValueRef::Real(f) => {
                            dyn_types[i].get_or_insert("real");
                            Some(fmt_real(f))
                        }
                        ValueRef::Text(t) => {
                            dyn_types[i].get_or_insert("text");
                            Some(String::from_utf8_lossy(t).into_owned())
                        }
                        ValueRef::Blob(b) => {
                            dyn_types[i].get_or_insert("blob");
                            Some(format!("x'{}'", hex(b).to_uppercase()))
                        }
                    };
                    vals.push(v);
                }
                out.rows.push(vals);
            }
        }
        out.columns = names
            .into_iter()
            .enumerate()
            .map(|(i, name)| Column {
                name,
                type_name: decl[i]
                    .clone()
                    .unwrap_or_else(|| dyn_types[i].unwrap_or("").to_string()),
            })
            .collect();
        out.rows_affected = if readonly {
            Some(out.rows.len() as u64)
        } else {
            Some(conn.changes())
        };
    }
    out.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(out)
}

fn rows_on(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> AppResult<Rows> {
    let mut stmt = conn.prepare(sql)?;
    let n = stmt.column_count();
    let mut out = Vec::new();
    let mut rows = stmt.query(params)?;
    while let Some(r) = rows.next()? {
        let mut v = Vec::with_capacity(n);
        for i in 0..n {
            v.push(match r.get_ref(i)? {
                ValueRef::Null => None,
                ValueRef::Integer(x) => Some(x.to_string()),
                ValueRef::Real(x) => Some(x.to_string()),
                ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
                ValueRef::Blob(b) => Some(hex(b)),
            });
        }
        out.push(v);
    }
    Ok(out)
}

fn s(v: &[Option<String>], i: usize) -> String {
    v.get(i).cloned().flatten().unwrap_or_default()
}
fn o(v: &[Option<String>], i: usize) -> Option<String> {
    v.get(i).cloned().flatten()
}

fn fk_list(conn: &Connection, schema: &str, table: &str) -> AppResult<Vec<ForeignKeyDef>> {
    let rows = rows_on(
        conn,
        "SELECT id, seq, \"table\", \"from\", \"to\", on_update, on_delete FROM pragma_foreign_key_list(?1, ?2) ORDER BY id, seq",
        &[&table, &schema],
    )?;
    let mut fks: Vec<(String, ForeignKeyDef)> = Vec::new();
    for r in rows {
        let id = s(&r, 0);
        let to = o(&r, 4);
        if let Some((_, fk)) = fks.iter_mut().find(|(i, _)| *i == id) {
            fk.columns.push(s(&r, 3));
            fk.ref_columns.push(to.unwrap_or_default());
        } else {
            fks.push((
                id.clone(),
                ForeignKeyDef {
                    name: format!("fk_{}_{}", table, id),
                    columns: vec![s(&r, 3)],
                    ref_schema: schema.to_string(),
                    ref_table: s(&r, 2),
                    ref_columns: vec![to.unwrap_or_default()],
                    on_update: s(&r, 5),
                    on_delete: s(&r, 6),
                },
            ));
        }
    }
    let mut out: Vec<ForeignKeyDef> = fks.into_iter().map(|(_, f)| f).collect();
    // `to` is NULL when the FK targets the parent's primary key — resolve it.
    for fk in out.iter_mut() {
        if fk.ref_columns.iter().any(|c| c.is_empty()) {
            let pk = rows_on(
                conn,
                "SELECT name FROM pragma_table_info(?1, ?2) WHERE pk > 0 ORDER BY pk",
                &[&fk.ref_table, &schema],
            )?;
            let pk: Vec<String> = pk.iter().map(|r| s(r, 0)).collect();
            if pk.len() == fk.columns.len() {
                fk.ref_columns = pk;
            }
        }
    }
    Ok(out)
}

impl LiteSession {
    pub fn open(cfg: &ConnectionConfig) -> AppResult<(LiteSession, ServerInfo)> {
        let path = expand_path(&cfg.file_path);
        let editor = open_conn(&path, cfg.read_only)?;
        let aux = open_conn(&path, cfg.read_only)?;
        let version: String = aux.query_row("SELECT sqlite_version()", [], |r| r.get(0))?;
        let editor_interrupt = editor.get_interrupt_handle();
        let file_name = path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "main".into());
        let info = ServerInfo {
            session_id: String::new(),
            connection_id: cfg.id.clone(),
            engine: Engine::Sqlite,
            flavor: "SQLite".into(),
            version,
            database: file_name,
            default_schema: "main".into(),
            user: String::new(),
            read_only: cfg.read_only,
        };
        Ok((
            LiteSession {
                editor: Arc::new(Mutex::new(editor)),
                aux: Arc::new(Mutex::new(aux)),
                editor_interrupt,
            },
            info,
        ))
    }

    async fn with<T: Send + 'static>(
        &self,
        editor: bool,
        f: impl FnOnce(&Connection) -> AppResult<T> + Send + 'static,
    ) -> AppResult<T> {
        let conn = if editor { self.editor.clone() } else { self.aux.clone() };
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| AppError::internal("SQLite connection lock poisoned"))?;
            f(&guard)
        })
        .await?
    }

    pub async fn run(&self, editor: bool, sql: &str, limit: usize) -> AppResult<QueryResult> {
        let sql = sql.to_string();
        self.with(editor, move |c| run_on(c, &sql, limit)).await
    }

    pub fn cancel(&self) {
        self.editor_interrupt.interrupt();
    }

    pub async fn transaction(&self, stmts: &[String], expect_one: bool) -> Result<Vec<QueryResult>, (usize, AppError)> {
        let stmts = stmts.to_vec();
        let res = self
            .with(false, move |c| {
                Ok((|| {
                    c.execute_batch("BEGIN").map_err(|e| (0usize, AppError::from(e)))?;
                    let mut results = Vec::new();
                    for (i, sql) in stmts.iter().enumerate() {
                        match run_on(c, sql, 1000).and_then(|r| super::check_one(expect_one, r)) {
                            Ok(r) => results.push(r),
                            Err(e) => {
                                let _ = c.execute_batch("ROLLBACK");
                                return Err((i, e));
                            }
                        }
                    }
                    c.execute_batch("COMMIT")
                        .map_err(|e| (stmts.len(), AppError::from(e)))?;
                    Ok(results)
                })())
            })
            .await;
        match res {
            Ok(inner) => inner,
            Err(e) => Err((0, e)),
        }
    }

    pub async fn schemas(&self) -> AppResult<Vec<String>> {
        self.with(false, |c| {
            let rows = rows_on(c, "SELECT name FROM pragma_database_list ORDER BY seq", &[])?;
            Ok(rows.iter().map(|r| s(r, 0)).collect())
        })
        .await
    }

    pub async fn objects(&self, schema: &str) -> AppResult<Vec<DbObject>> {
        let schema = schema.to_string();
        self.with(false, move |c| {
            let sql = format!(
                "SELECT name, type FROM {}.sqlite_master WHERE type IN ('table', 'view')
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
                quote_ident(Engine::Sqlite, &schema)
            );
            let rows = rows_on(c, &sql, &[])?;
            Ok(rows
                .iter()
                .map(|r| DbObject {
                    name: s(r, 0),
                    kind: s(r, 1),
                    rows_estimate: None,
                    comment: None,
                    signature: None,
                })
                .collect())
        })
        .await
    }

    pub async fn structure(&self, schema: &str, table: &str) -> AppResult<TableStructure> {
        let (schema, table) = (schema.to_string(), table.to_string());
        self.with(false, move |c| {
            let master = format!("{}.sqlite_master", quote_ident(Engine::Sqlite, &schema));
            let cols = rows_on(
                c,
                "SELECT name, type, \"notnull\", dflt_value, pk, hidden FROM pragma_table_xinfo(?1, ?2) ORDER BY cid",
                &[&table, &schema],
            )?;
            let mut pk_cols: Vec<(i64, String)> = cols
                .iter()
                .filter_map(|r| {
                    let pk: i64 = s(r, 4).parse().unwrap_or(0);
                    (pk > 0).then(|| (pk, s(r, 0)))
                })
                .collect();
            pk_cols.sort();
            let primary_key: Vec<String> = pk_cols.into_iter().map(|(_, n)| n).collect();
            let columns = cols
                .iter()
                .map(|r| {
                    let hidden = s(r, 5);
                    ColumnDef {
                        name: s(r, 0),
                        data_type: s(r, 1),
                        nullable: s(r, 2) == "0",
                        default_value: o(r, 3),
                        is_primary_key: primary_key.contains(&s(r, 0)),
                        comment: None,
                        extra: match hidden.as_str() {
                            "2" | "3" => Some("generated".into()),
                            _ => None,
                        },
                    }
                })
                .collect();
            let idx_rows = rows_on(
                c,
                "SELECT name, \"unique\", origin FROM pragma_index_list(?1, ?2)",
                &[&table, &schema],
            )?;
            let mut indexes = Vec::new();
            for r in idx_rows {
                let name = s(&r, 0);
                let icols = rows_on(
                    c,
                    "SELECT name FROM pragma_index_info(?1, ?2) ORDER BY seqno",
                    &[&name, &schema],
                )?;
                let def = rows_on(
                    c,
                    &format!("SELECT sql FROM {master} WHERE type = 'index' AND name = ?1"),
                    &[&name],
                )?;
                indexes.push(IndexDef {
                    columns: icols.iter().map(|r| s(r, 0)).collect(),
                    unique: s(&r, 1) == "1",
                    primary: s(&r, 2) == "pk",
                    definition: def.first().and_then(|d| o(d, 0)),
                    name,
                });
            }
            if !primary_key.is_empty() && !indexes.iter().any(|i| i.primary) {
                indexes.insert(
                    0,
                    IndexDef {
                        name: "PRIMARY KEY".into(),
                        columns: primary_key.clone(),
                        unique: true,
                        primary: true,
                        definition: None,
                    },
                );
            }
            let foreign_keys = fk_list(c, &schema, &table)?;
            let mut referenced_by = Vec::new();
            let tables = rows_on(
                c,
                &format!("SELECT name FROM {master} WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"),
                &[],
            )?;
            for t in tables {
                let other = s(&t, 0);
                for fk in fk_list(c, &schema, &other)? {
                    if fk.ref_table.eq_ignore_ascii_case(&table) {
                        referenced_by.push(ForeignKeyDef {
                            name: format!("{other}.{}", fk.name),
                            columns: fk.ref_columns.clone(),
                            ref_schema: schema.clone(),
                            ref_table: other.clone(),
                            ref_columns: fk.columns.clone(),
                            on_update: fk.on_update.clone(),
                            on_delete: fk.on_delete.clone(),
                        });
                    }
                }
            }
            let triggers = rows_on(
                c,
                &format!("SELECT name, sql FROM {master} WHERE type = 'trigger' AND tbl_name = ?1"),
                &[&table],
            )?
            .iter()
            .map(|r| TriggerDef {
                name: s(r, 0),
                definition: s(r, 1),
            })
            .collect();
            let constraints = indexes
                .iter()
                .filter(|i| i.unique)
                .map(|i| ConstraintDef {
                    name: i.name.clone(),
                    kind: if i.primary { "primary" } else { "unique" }.into(),
                    definition: format!(
                        "{} ({})",
                        if i.primary { "PRIMARY KEY" } else { "UNIQUE" },
                        i.columns.join(", ")
                    ),
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
        })
        .await
    }

    pub async fn ddl(&self, schema: &str, name: &str) -> AppResult<String> {
        let (schema, name) = (schema.to_string(), name.to_string());
        self.with(false, move |c| {
            let master = format!("{}.sqlite_master", quote_ident(Engine::Sqlite, &schema));
            let rows = rows_on(
                c,
                &format!(
                    "SELECT sql FROM {master} WHERE (name = ?1 OR (tbl_name = ?1 AND type IN ('index', 'trigger')))
                     AND sql IS NOT NULL ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'view' THEN 0 WHEN 'index' THEN 1 ELSE 2 END, name"
                ),
                &[&name],
            )?;
            Ok(rows
                .iter()
                .map(|r| format!("{};", s(r, 0)))
                .collect::<Vec<_>>()
                .join("\n\n")
                + "\n")
        })
        .await
    }

    pub async fn relations(&self, schema: &str) -> AppResult<SchemaRelations> {
        let schema = schema.to_string();
        self.with(false, move |c| {
            let master = format!("{}.sqlite_master", quote_ident(Engine::Sqlite, &schema));
            let tables = rows_on(
                c,
                &format!("SELECT name FROM {master} WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name"),
                &[],
            )?;
            let mut out = SchemaRelations::default();
            for t in tables {
                let table = s(&t, 0);
                let mut pk: Vec<(i64, String)> = rows_on(
                    c,
                    "SELECT pk, name FROM pragma_table_info(?1, ?2) WHERE pk > 0",
                    &[&table, &schema],
                )?
                .iter()
                .map(|r| (s(r, 0).parse().unwrap_or(0), s(r, 1)))
                .collect();
                pk.sort();
                if !pk.is_empty() {
                    out.primary_keys.push(TablePrimaryKey {
                        table: table.clone(),
                        columns: pk.into_iter().map(|(_, n)| n).collect(),
                    });
                }
                for fk in fk_list(c, &schema, &table)? {
                    out.foreign_keys.push(ForeignKeyRelation {
                        name: fk.name,
                        schema: schema.clone(),
                        table: table.clone(),
                        columns: fk.columns,
                        ref_schema: schema.clone(),
                        ref_table: fk.ref_table,
                        ref_columns: fk.ref_columns,
                    });
                }
            }
            Ok(out)
        })
        .await
    }

    pub async fn completions(&self) -> AppResult<Vec<CompletionTable>> {
        self.with(false, |c| {
            let tables = rows_on(
                c,
                "SELECT name, type FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
                &[],
            )?;
            let mut out = Vec::new();
            for t in tables {
                let name = s(&t, 0);
                let cols = rows_on(c, "SELECT name, type FROM pragma_table_info(?1)", &[&name])?;
                out.push(CompletionTable {
                    schema: "main".into(),
                    name,
                    kind: s(&t, 1),
                    columns: cols.iter().map(|r| (s(r, 0), s(r, 1))).collect(),
                });
            }
            Ok(out)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_queries() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, score REAL, data BLOB);
             INSERT INTO t (name, score, data) VALUES ('a', 1.0, x'0102'), (NULL, 2.5, NULL), ('c', 3, NULL);",
        )
        .unwrap();
        let r = run_on(&c, "SELECT * FROM t ORDER BY id", 2).unwrap();
        assert!(r.has_rows);
        assert!(r.truncated);
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.columns[0].type_name, "integer");
        assert_eq!(r.rows[0][2].as_deref(), Some("1.0"));
        assert_eq!(r.rows[0][3].as_deref(), Some("x'0102'"));
        assert_eq!(r.rows[1][1], None);
        let u = run_on(&c, "UPDATE t SET score = 0", 10).unwrap();
        assert_eq!(u.rows_affected, Some(3));
        assert!(!u.has_rows);
        let ret = run_on(&c, "INSERT INTO t (name) VALUES ('x'), ('y') RETURNING id", 1).unwrap();
        assert_eq!(ret.rows.len(), 1);
        let count = run_on(&c, "SELECT count(*) FROM t", 10).unwrap();
        assert_eq!(count.rows[0][0].as_deref(), Some("5"));
    }
}
