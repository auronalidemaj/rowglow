//! DuckDB driver (bundled DuckDB, no system dependency). Like SQLite it opens a
//! local file, but a DuckDB file can only be opened once per process, so the
//! browsing connection is a clone of the editor connection on the same
//! database instance. Results come back as Arrow batches and are rendered to
//! text with Arrow's formatter, which handles every DuckDB type (lists,
//! structs, maps, intervals, decimals, ...).

use crate::error::{AppError, AppResult};
use crate::model::*;
use crate::util::{first_keyword, hex};
use duckdb::arrow::array::{Array, BinaryArray, LargeBinaryArray};
use duckdb::arrow::datatypes::DataType;
use duckdb::arrow::util::display::{ArrayFormatter, FormatOptions};
use duckdb::{AccessMode, Config, Connection, InterruptHandle, ToSql};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Instant;

pub struct DuckSession {
    /// Keeps the file's shared database instance registered while open.
    _shared: Option<Arc<Shared>>,
    editor: Arc<Mutex<Connection>>,
    aux: Arc<Mutex<Connection>>,
    editor_interrupt: Arc<InterruptHandle>,
}

type Rows = Vec<Vec<Option<String>>>;

pub const MEMORY: &str = ":memory:";

/// One DuckDB database instance per file and process. Two instances on the
/// same file would both write to it (file locks don't stop a process from
/// locking itself), so later sessions on a file clone a connection from the
/// instance that is already open.
struct Shared {
    root: Mutex<Connection>,
    read_only: bool,
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Weak<Shared>>> {
    static OPEN: OnceLock<Mutex<HashMap<PathBuf, Weak<Shared>>>> = OnceLock::new();
    OPEN.get_or_init(|| Mutex::new(HashMap::new()))
}

fn open_db(path: &str, read_only: bool) -> AppResult<(Option<Arc<Shared>>, Connection)> {
    if path.trim() == MEMORY || path.trim().is_empty() {
        return Ok((None, Connection::open_in_memory()?));
    }
    let p = super::sqlite::expand_path(path);
    if !p.exists() {
        return Err(AppError::new("connection", format!("Database file not found: {}", p.display())));
    }
    let key = p.canonicalize().unwrap_or_else(|_| p.clone());
    let mut open = registry().lock().map_err(|_| AppError::internal("DuckDB registry lock poisoned"))?;
    open.retain(|_, w| w.strong_count() > 0);
    if let Some(shared) = open.get(&key).and_then(Weak::upgrade) {
        if shared.read_only != read_only {
            return Err(AppError::new(
                "connection",
                format!(
                    "{} is already open {} in Rowglow. Close that connection first — DuckDB opens a file in one mode at a time.",
                    p.display(),
                    if shared.read_only { "read-only" } else { "with write access" }
                ),
            ));
        }
        let conn = shared.root.lock().map_err(|_| AppError::internal("DuckDB lock poisoned"))?.try_clone()?;
        return Ok((Some(shared), conn));
    }
    let mut config = Config::default();
    if read_only {
        config = config.access_mode(AccessMode::ReadOnly)?;
    }
    let root = Connection::open_with_flags(&p, config).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("lock") {
            AppError::new(
                "connection",
                format!("{} is open in another program. DuckDB lets only one program write to a file at a time — close it there, or connect read-only.", p.display()),
            )
        } else {
            AppError::from(e)
        }
    })?;
    let conn = root.try_clone()?;
    let shared = Arc::new(Shared { root: Mutex::new(root), read_only });
    open.insert(key, Arc::downgrade(&shared));
    Ok((Some(shared), conn))
}

/// Creates an empty DuckDB database file ("New DuckDB database…").
pub fn create_file(path: &str) -> AppResult<()> {
    let p = super::sqlite::expand_path(path);
    if p.exists() {
        return Err(AppError::invalid("A file already exists at that path"));
    }
    let conn = Connection::open(&p)?;
    conn.execute_batch("CHECKPOINT")?;
    Ok(())
}

/// SQL-ish names for Arrow types so the grid can align numbers, and so on.
fn type_name(dt: &DataType) -> String {
    match dt {
        DataType::Boolean => "boolean".into(),
        DataType::Int8 => "tinyint".into(),
        DataType::Int16 => "smallint".into(),
        DataType::Int32 => "integer".into(),
        DataType::Int64 => "bigint".into(),
        DataType::UInt8 => "utinyint".into(),
        DataType::UInt16 => "usmallint".into(),
        DataType::UInt32 => "uinteger".into(),
        DataType::UInt64 => "ubigint".into(),
        DataType::Float16 | DataType::Float32 => "float".into(),
        DataType::Float64 => "double".into(),
        DataType::Decimal32(p, s) | DataType::Decimal64(p, s) | DataType::Decimal128(p, s) | DataType::Decimal256(p, s) => {
            format!("decimal({p},{s})")
        }
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => "varchar".into(),
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView | DataType::FixedSizeBinary(_) => "blob".into(),
        DataType::Date32 | DataType::Date64 => "date".into(),
        DataType::Time32(_) | DataType::Time64(_) => "time".into(),
        DataType::Timestamp(_, Some(_)) => "timestamptz".into(),
        DataType::Timestamp(_, None) => "timestamp".into(),
        DataType::Interval(_) | DataType::Duration(_) => "interval".into(),
        DataType::List(f) | DataType::LargeList(f) | DataType::ListView(f) => format!("{}[]", type_name(f.data_type())),
        DataType::FixedSizeList(f, n) => format!("{}[{n}]", type_name(f.data_type())),
        DataType::Struct(_) => "struct".into(),
        DataType::Map(_, _) => "map".into(),
        DataType::Union(_, _) => "union".into(),
        DataType::Dictionary(_, v) => type_name(v),
        DataType::Null => "null".into(),
        other => format!("{other}").to_lowercase(),
    }
}

const TS: &str = "%Y-%m-%d %H:%M:%S%.f";
const TS_TZ: &str = "%Y-%m-%d %H:%M:%S%.f%:z";

fn format_options() -> FormatOptions<'static> {
    FormatOptions::new()
        .with_timestamp_format(Some(TS))
        .with_timestamp_tz_format(Some(TS_TZ))
        .with_datetime_format(Some(TS))
        .with_display_error(true)
}

/// Renders one column of a batch. Blobs are shown as 0x… hex (the grid sends
/// them back as `from_hex(…)`), everything else via Arrow's formatter.
fn column_values(array: &dyn Array, out: &mut [Vec<Option<String>>]) -> AppResult<()> {
    if let Some(b) = array.as_any().downcast_ref::<BinaryArray>() {
        for (i, row) in out.iter_mut().enumerate() {
            row.push((!b.is_null(i)).then(|| format!("0x{}", hex(b.value(i)).to_uppercase())));
        }
        return Ok(());
    }
    if let Some(b) = array.as_any().downcast_ref::<LargeBinaryArray>() {
        for (i, row) in out.iter_mut().enumerate() {
            row.push((!b.is_null(i)).then(|| format!("0x{}", hex(b.value(i)).to_uppercase())));
        }
        return Ok(());
    }
    let opts = format_options();
    let f = ArrayFormatter::try_new(array, &opts).map_err(|e| AppError::new("db", e.to_string()))?;
    for (i, row) in out.iter_mut().enumerate() {
        row.push((!array.is_null(i)).then(|| f.value(i).to_string()));
    }
    Ok(())
}

/// Statements that report a row count instead of returning rows.
fn is_dml(sql: &str) -> bool {
    matches!(first_keyword(sql).as_str(), "INSERT" | "UPDATE" | "DELETE")
        && !crate::safety::strip(sql).to_ascii_uppercase().contains("RETURNING")
}

/// DDL, settings, ATTACH, COPY …: DuckDB answers with a single "Count" column
/// (or none) that isn't a real result set.
fn is_command(sql: &str) -> bool {
    !matches!(
        first_keyword(sql).as_str(),
        "SELECT" | "WITH" | "FROM" | "VALUES" | "TABLE" | "SHOW" | "DESCRIBE" | "DESC" | "SUMMARIZE" | "PRAGMA"
            | "EXPLAIN" | "CALL" | "PIVOT" | "UNPIVOT" | "INSERT" | "UPDATE" | "DELETE"
    )
}

pub fn run_on(conn: &Connection, sql: &str, limit: usize) -> AppResult<QueryResult> {
    let start = Instant::now();
    let mut out = QueryResult::default();
    let mut stmt = conn.prepare(sql)?;
    let dml = is_dml(sql);
    let stream = stmt.stream_arrow([])?;
    let schema = stream.get_schema();
    let fields = schema.fields();
    // INSERT/UPDATE/DELETE return a single "Count" column with the number of rows.
    let count_only = fields.len() == 1 && fields[0].name() == "Count";
    if count_only && !dml && is_command(sql) {
        for batch in stream {
            batch?;
        }
        out.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        return Ok(out);
    }
    if dml && count_only {
        let mut count = 0u64;
        for batch in stream {
            let batch = batch?;
            let mut rows: Rows = vec![Vec::new(); batch.num_rows()];
            column_values(batch.column(0).as_ref(), &mut rows)?;
            for r in rows {
                count += r.first().cloned().flatten().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            }
        }
        out.rows_affected = Some(count);
        out.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        return Ok(out);
    }
    out.columns = fields
        .iter()
        .map(|f| Column {
            name: f.name().clone(),
            type_name: type_name(f.data_type()),
        })
        .collect();
    if fields.is_empty() {
        // DDL and settings produce no result set.
        for batch in stream {
            batch?;
        }
        out.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        return Ok(out);
    }
    out.has_rows = true;
    for batch in stream {
        let batch = batch?;
        let n = batch.num_rows();
        if n == 0 {
            continue;
        }
        if out.rows.len() >= limit {
            out.truncated = true;
            break;
        }
        let mut rows: Rows = vec![Vec::with_capacity(fields.len()); n];
        for c in 0..batch.num_columns() {
            column_values(batch.column(c).as_ref(), &mut rows)?;
        }
        let room = limit - out.rows.len();
        if rows.len() > room {
            rows.truncate(room);
            out.truncated = true;
        }
        out.rows.extend(rows);
        if out.truncated {
            break;
        }
    }
    out.rows_affected = Some(out.rows.len() as u64);
    out.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(out)
}

/// Metadata helper: runs a parameterised query and renders every value as text.
fn rows_on(conn: &Connection, sql: &str, params: &[&dyn ToSql]) -> AppResult<Rows> {
    let mut stmt = conn.prepare(sql)?;
    let stream = stmt.stream_arrow(params)?;
    let mut out = Vec::new();
    for batch in stream {
        let batch = batch?;
        let mut rows: Rows = vec![Vec::new(); batch.num_rows()];
        for c in 0..batch.num_columns() {
            column_values(batch.column(c).as_ref(), &mut rows)?;
        }
        out.extend(rows);
    }
    Ok(out)
}

fn s(v: &[Option<String>], i: usize) -> String {
    v.get(i).cloned().flatten().unwrap_or_default()
}
fn o(v: &[Option<String>], i: usize) -> Option<String> {
    v.get(i).cloned().flatten()
}
/// Columns selected with `to_json(list)` come back as a JSON array of strings.
fn list(v: &[Option<String>], i: usize) -> Vec<String> {
    o(v, i)
        .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
        .unwrap_or_default()
}

impl DuckSession {
    pub fn open(cfg: &ConnectionConfig) -> AppResult<(DuckSession, ServerInfo)> {
        let (shared, editor) = open_db(&cfg.file_path, cfg.read_only)?;
        let aux = editor.try_clone()?;
        let version = rows_on(&aux, "SELECT version()", &[])?
            .first()
            .map(|r| s(r, 0))
            .unwrap_or_default();
        let database = rows_on(&aux, "SELECT current_database()", &[])?
            .first()
            .map(|r| s(r, 0))
            .unwrap_or_else(|| "memory".into());
        let editor_interrupt = editor.interrupt_handle();
        let info = ServerInfo {
            session_id: String::new(),
            connection_id: cfg.id.clone(),
            engine: Engine::Duckdb,
            flavor: "DuckDB".into(),
            version: version.trim_start_matches('v').to_string(),
            database,
            default_schema: "main".into(),
            user: String::new(),
            read_only: cfg.read_only,
        };
        Ok((
            DuckSession {
                _shared: shared,
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
                .map_err(|_| AppError::internal("DuckDB connection lock poisoned"))?;
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
                    c.execute_batch("BEGIN TRANSACTION").map_err(|e| (0usize, AppError::from(e)))?;
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
                    c.execute_batch("COMMIT").map_err(|e| (stmts.len(), AppError::from(e)))?;
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
            let rows = rows_on(
                c,
                "SELECT schema_name FROM duckdb_schemas() WHERE database_name = current_database() AND NOT internal
                 ORDER BY schema_name = 'main' DESC, schema_name",
                &[],
            )?;
            Ok(rows.iter().map(|r| s(r, 0)).collect())
        })
        .await
    }

    pub async fn objects(&self, schema: &str) -> AppResult<Vec<DbObject>> {
        let schema = schema.to_string();
        self.with(false, move |c| {
            let rows = rows_on(
                c,
                "SELECT table_name, 'table', estimated_size, comment FROM duckdb_tables()
                   WHERE database_name = current_database() AND schema_name = ?1 AND NOT internal
                 UNION ALL
                 SELECT view_name, 'view', NULL, comment FROM duckdb_views()
                   WHERE database_name = current_database() AND schema_name = ?1 AND NOT internal
                 UNION ALL
                 SELECT sequence_name, 'sequence', NULL, comment FROM duckdb_sequences()
                   WHERE database_name = current_database() AND schema_name = ?1
                 UNION ALL
                 SELECT DISTINCT function_name, 'function', NULL, NULL FROM duckdb_functions()
                   WHERE database_name = current_database() AND schema_name = ?1 AND NOT internal AND function_type IN ('macro', 'table_macro')
                 ORDER BY 1",
                &[&schema],
            )?;
            Ok(rows
                .iter()
                .map(|r| DbObject {
                    name: s(r, 0),
                    kind: s(r, 1),
                    rows_estimate: o(r, 2).and_then(|v| v.parse().ok()),
                    comment: o(r, 3).filter(|c| !c.is_empty()),
                    signature: None,
                })
                .collect())
        })
        .await
    }

    pub async fn structure(&self, schema: &str, table: &str) -> AppResult<TableStructure> {
        let (schema, table) = (schema.to_string(), table.to_string());
        self.with(false, move |c| {
            let cols = rows_on(
                c,
                "SELECT column_name, data_type, is_nullable, column_default, comment FROM duckdb_columns()
                 WHERE database_name = current_database() AND schema_name = ?1 AND table_name = ?2
                 ORDER BY column_index",
                &[&schema, &table],
            )?;
            let cons = rows_on(
                c,
                "SELECT constraint_type, coalesce(constraint_name, ''), constraint_text, to_json(constraint_column_names),
                        referenced_table, to_json(referenced_column_names)
                 FROM duckdb_constraints()
                 WHERE database_name = current_database() AND schema_name = ?1 AND table_name = ?2
                 ORDER BY constraint_index",
                &[&schema, &table],
            )?;
            let primary_key: Vec<String> = cons
                .iter()
                .find(|r| s(r, 0) == "PRIMARY KEY")
                .map(|r| list(r, 3))
                .unwrap_or_default();
            let columns = cols
                .iter()
                .map(|r| ColumnDef {
                    name: s(r, 0),
                    data_type: s(r, 1).to_lowercase(),
                    nullable: s(r, 2) == "true",
                    default_value: o(r, 3),
                    is_primary_key: primary_key.contains(&s(r, 0)),
                    comment: o(r, 4).filter(|c| !c.is_empty()),
                    extra: None,
                })
                .collect();
            let fk_name = |r: &Vec<Option<String>>, i: usize| {
                let n = s(r, 1);
                if n.is_empty() {
                    format!("fk_{}_{}", table, i)
                } else {
                    n
                }
            };
            let foreign_keys: Vec<ForeignKeyDef> = cons
                .iter()
                .filter(|r| s(r, 0) == "FOREIGN KEY")
                .enumerate()
                .map(|(i, r)| ForeignKeyDef {
                    name: fk_name(r, i),
                    columns: list(r, 3),
                    ref_schema: schema.clone(),
                    ref_table: s(r, 4),
                    ref_columns: list(r, 5),
                    on_update: "NO ACTION".into(),
                    on_delete: "NO ACTION".into(),
                })
                .collect();
            let constraints = cons
                .iter()
                .filter(|r| matches!(s(r, 0).as_str(), "PRIMARY KEY" | "UNIQUE" | "CHECK"))
                .map(|r| ConstraintDef {
                    name: if s(r, 1).is_empty() { s(r, 0).to_lowercase() } else { s(r, 1) },
                    kind: match s(r, 0).as_str() {
                        "PRIMARY KEY" => "primary",
                        "UNIQUE" => "unique",
                        _ => "check",
                    }
                    .into(),
                    definition: s(r, 2),
                })
                .collect();
            let mut indexes: Vec<IndexDef> = rows_on(
                c,
                "SELECT index_name, is_unique, is_primary, expressions, sql FROM duckdb_indexes()
                 WHERE database_name = current_database() AND schema_name = ?1 AND table_name = ?2 ORDER BY index_name",
                &[&schema, &table],
            )?
            .iter()
            .map(|r| IndexDef {
                name: s(r, 0),
                columns: o(r, 3)
                    .map(|e| {
                        e.trim_matches(|ch| ch == '[' || ch == ']')
                            .split(',')
                            .map(|x| x.trim().trim_matches('\'').trim_matches('"').to_string())
                            .filter(|x| !x.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
                unique: s(r, 1) == "true",
                primary: s(r, 2) == "true",
                definition: o(r, 4),
            })
            .collect();
            if !primary_key.is_empty() {
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
            let referenced_by = rows_on(
                c,
                "SELECT table_name, coalesce(constraint_name, ''), to_json(constraint_column_names), to_json(referenced_column_names)
                 FROM duckdb_constraints()
                 WHERE database_name = current_database() AND schema_name = ?1 AND constraint_type = 'FOREIGN KEY'
                   AND referenced_table = ?2",
                &[&schema, &table],
            )?
            .iter()
            .map(|r| ForeignKeyDef {
                name: if s(r, 1).is_empty() { format!("fk_{}", s(r, 0)) } else { s(r, 1) },
                columns: list(r, 3),
                ref_schema: schema.clone(),
                ref_table: s(r, 0),
                ref_columns: list(r, 2),
                on_update: "NO ACTION".into(),
                on_delete: "NO ACTION".into(),
            })
            .collect();
            Ok(TableStructure {
                columns,
                primary_key,
                indexes,
                foreign_keys,
                constraints,
                triggers: Vec::new(),
                referenced_by,
            })
        })
        .await
    }

    pub async fn ddl(&self, schema: &str, name: &str) -> AppResult<String> {
        let (schema, name) = (schema.to_string(), name.to_string());
        self.with(false, move |c| {
            let rows = rows_on(
                c,
                "SELECT sql, 0 FROM duckdb_tables() WHERE database_name = current_database() AND schema_name = ?1 AND table_name = ?2
                 UNION ALL SELECT sql, 0 FROM duckdb_views() WHERE database_name = current_database() AND schema_name = ?1 AND view_name = ?2
                 UNION ALL SELECT sql, 0 FROM duckdb_sequences() WHERE database_name = current_database() AND schema_name = ?1 AND sequence_name = ?2
                 UNION ALL SELECT sql, 1 FROM duckdb_indexes() WHERE database_name = current_database() AND schema_name = ?1 AND table_name = ?2 AND sql IS NOT NULL
                 UNION ALL SELECT macro_definition, 0 FROM duckdb_functions() WHERE database_name = current_database() AND schema_name = ?1 AND function_name = ?2 AND function_type IN ('macro', 'table_macro')
                 ORDER BY 2",
                &[&schema, &name],
            )?;
            let mut out: Vec<String> = Vec::new();
            for r in rows {
                let mut sql = s(&r, 0).trim().to_string();
                if sql.is_empty() {
                    continue;
                }
                if !sql.ends_with(';') {
                    sql.push(';');
                }
                out.push(sql);
            }
            Ok(out.join("\n\n") + "\n")
        })
        .await
    }

    pub async fn completions(&self) -> AppResult<Vec<CompletionTable>> {
        self.with(false, |c| {
            let rows = rows_on(
                c,
                "SELECT c.schema_name, c.table_name, CASE WHEN v.view_name IS NULL THEN 'table' ELSE 'view' END, c.column_name, c.data_type
                 FROM duckdb_columns() c
                 LEFT JOIN duckdb_views() v ON v.database_name = c.database_name AND v.schema_name = c.schema_name AND v.view_name = c.table_name
                 WHERE c.database_name = current_database() AND NOT c.internal
                 ORDER BY c.schema_name, c.table_name, c.column_index",
                &[],
            )?;
            let mut out: Vec<CompletionTable> = Vec::new();
            for r in rows {
                let (schema, name) = (s(&r, 0), s(&r, 1));
                match out.last_mut() {
                    Some(t) if t.schema == schema && t.name == name => t.columns.push((s(&r, 3), s(&r, 4).to_lowercase())),
                    _ => out.push(CompletionTable {
                        schema,
                        name,
                        kind: s(&r, 2),
                        columns: vec![(s(&r, 3), s(&r, 4).to_lowercase())],
                    }),
                }
            }
            Ok(out)
        })
        .await
    }

    pub async fn relations(&self, schema: &str) -> AppResult<SchemaRelations> {
        let schema = schema.to_string();
        self.with(false, move |c| {
            let rows = rows_on(
                c,
                "SELECT table_name, constraint_type, coalesce(constraint_name, ''), to_json(constraint_column_names),
                        referenced_table, to_json(referenced_column_names)
                 FROM duckdb_constraints()
                 WHERE database_name = current_database() AND schema_name = ?1 AND constraint_type IN ('FOREIGN KEY', 'PRIMARY KEY')",
                &[&schema],
            )?;
            let mut out = SchemaRelations::default();
            for r in rows {
                if s(&r, 1) == "PRIMARY KEY" {
                    out.primary_keys.push(TablePrimaryKey { table: s(&r, 0), columns: list(&r, 3) });
                } else {
                    out.foreign_keys.push(ForeignKeyRelation {
                        name: s(&r, 2),
                        schema: schema.clone(),
                        table: s(&r, 0),
                        columns: list(&r, 3),
                        ref_schema: schema.clone(),
                        ref_table: s(&r, 4),
                        ref_columns: list(&r, 5),
                    });
                }
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
    fn reads_csv_parquet_and_json_files() {
        let dir = std::env::temp_dir().join(format!("rg-duck-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("a.csv");
        std::fs::write(&csv, "id,name\n1,Zoë\n2,Bob\n").unwrap();
        let json = dir.join("a.json");
        std::fs::write(&json, r#"[{"id": 1, "tags": ["x"]}, {"id": 2, "tags": []}]"#).unwrap();
        let pq = dir.join("a.parquet");
        let c = Connection::open_in_memory().unwrap();
        let q = |p: &std::path::Path| p.to_string_lossy().replace('\'', "''");
        let r = run_on(&c, &format!("SELECT count(*) FROM read_csv('{}')", q(&csv)), 10).unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("2"));
        run_on(&c, &format!("COPY (SELECT * FROM read_csv('{}')) TO '{}' (FORMAT parquet)", q(&csv), q(&pq)), 10).unwrap();
        let r = run_on(&c, &format!("SELECT name FROM read_parquet('{}') ORDER BY id", q(&pq)), 10).unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("Zoë"));
        let r = run_on(&c, &format!("SELECT tags FROM read_json('{}') ORDER BY id", q(&json)), 10).unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("[x]"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_instance_per_file() {
        let dir = std::env::temp_dir().join(format!("rg-duck-share-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("s.duckdb");
        let _ = std::fs::remove_file(&f);
        create_file(f.to_str().unwrap()).unwrap();
        let path = f.to_str().unwrap().to_string();
        let (a, ca) = open_db(&path, false).unwrap();
        let (b, cb) = open_db(&path, false).unwrap();
        assert!(Arc::ptr_eq(a.as_ref().unwrap(), b.as_ref().unwrap()), "second open shares the instance");
        ca.execute_batch("CREATE TABLE t (x INT); INSERT INTO t VALUES (1)").unwrap();
        let r = run_on(&cb, "SELECT count(*) FROM t", 1).unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("1"));
        assert!(open_db(&path, true).is_err(), "mode mismatch is refused");
        drop((a, b, ca, cb));
        let (c, _) = open_db(&path, true).unwrap();
        assert!(c.unwrap().read_only, "fresh instance after all sessions closed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runs_queries() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name VARCHAR, score DECIMAL(5,2), data BLOB, ts TIMESTAMP, tags VARCHAR[]);
             INSERT INTO t VALUES (1, 'a', 1.5, '\\x01\\x02'::BLOB, '2024-05-01 10:20:30', ['x', 'y']), (2, NULL, 2, NULL, NULL, NULL), (3, 'c', 3, NULL, NULL, []);",
        )
        .unwrap();
        let r = run_on(&c, "SELECT * FROM t ORDER BY id", 2).unwrap();
        assert!(r.has_rows);
        assert!(r.truncated);
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.columns[0].type_name, "integer");
        assert_eq!(r.columns[2].type_name, "decimal(5,2)");
        assert_eq!(r.rows[0][2].as_deref(), Some("1.50"));
        assert_eq!(r.rows[0][3].as_deref(), Some("0x0102"));
        assert_eq!(r.rows[0][4].as_deref(), Some("2024-05-01 10:20:30"));
        assert_eq!(r.rows[0][5].as_deref(), Some("[x, y]"));
        assert_eq!(r.rows[1][1], None);
        let u = run_on(&c, "UPDATE t SET score = 0", 10).unwrap();
        assert_eq!(u.rows_affected, Some(3));
        assert!(!u.has_rows);
        let ret = run_on(&c, "INSERT INTO t (id, name) VALUES (4, 'x'), (5, 'y') RETURNING id", 1).unwrap();
        assert!(ret.has_rows);
        assert_eq!(ret.rows.len(), 1);
        let ddl = run_on(&c, "CREATE TABLE u (a INT)", 10).unwrap();
        assert!(!ddl.has_rows);
        let count = run_on(&c, "SELECT count(*) FROM t", 10).unwrap();
        assert_eq!(count.rows[0][0].as_deref(), Some("5"));
        let blob = run_on(&c, "SELECT data FROM t WHERE data = from_hex('0102')", 10).unwrap();
        assert_eq!(blob.rows.len(), 1);
    }
}
