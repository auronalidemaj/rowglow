//! CSV import: preview a file (delimiter, header and column types are
//! detected), then insert it into an existing or new table.
//!
//! The import runs in ONE transaction on its own connection, so it either
//! lands completely or not at all, and never mixes with the user's editor
//! session. Rows go in as multi-row INSERTs of `BATCH` rows with every value
//! sent as a quoted literal (the database casts it to the column type).

use crate::drivers::{Channel, Session};
use crate::error::{AppError, AppResult};
use crate::model::Engine;
use crate::util::{qualified, quote_ident, quote_literal};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const BATCH: usize = 500;
const PREVIEW_ROWS: usize = 50;
const SAMPLE_ROWS: usize = 2000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvColumn {
    pub name: String,
    /// integer | decimal | boolean | date | timestamp | text
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvPreview {
    pub path: String,
    pub delimiter: String,
    pub has_header: bool,
    pub encoding: String,
    pub columns: Vec<CsvColumn>,
    pub rows: Vec<Vec<String>>,
    pub total_rows: u64,
    pub file_size: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportColumn {
    /// Index of the CSV column.
    pub source: usize,
    /// Target column name.
    pub target: String,
    /// SQL type, only used when creating the table.
    #[serde(default)]
    pub sql_type: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
    pub session_id: String,
    pub job_id: String,
    pub path: String,
    pub delimiter: String,
    pub has_header: bool,
    #[serde(default)]
    pub encoding: String,
    pub schema: String,
    pub table: String,
    /// Create the table first (fails if it exists).
    #[serde(default)]
    pub create: bool,
    pub columns: Vec<ImportColumn>,
    /// Empty cells become NULL instead of ''.
    #[serde(default = "yes")]
    pub empty_as_null: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Default)]
pub struct ImportJob {
    pub rows_done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

fn jobs() -> &'static Mutex<HashMap<String, Arc<ImportJob>>> {
    static JOBS: OnceLock<Mutex<HashMap<String, Arc<ImportJob>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn progress(job_id: &str) -> Option<(u64, u64)> {
    jobs()
        .lock()
        .ok()?
        .get(job_id)
        .map(|j| (j.rows_done.load(Ordering::Relaxed), j.total.load(Ordering::Relaxed)))
}

pub fn cancel(job_id: &str) {
    if let Some(j) = jobs().lock().ok().and_then(|m| m.get(job_id).cloned()) {
        j.cancel.store(true, Ordering::Relaxed);
    }
}

// ------------------------------------------------------------------ reading

/// Reads the whole file as text: UTF-8 (BOM stripped) or, if that fails,
/// Windows-1252/Latin-1 so Excel exports don't come out garbled.
fn read_text(path: &Path, limit: Option<u64>) -> AppResult<(String, &'static str)> {
    let f = File::open(path).map_err(|e| AppError::invalid(format!("Couldn't open {}: {e}", path.display())))?;
    let mut bytes = Vec::new();
    match limit {
        Some(n) => {
            f.take(n).read_to_end(&mut bytes)?;
        }
        None => {
            BufReader::new(f).read_to_end(&mut bytes)?;
        }
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        bytes.drain(..3);
    }
    match String::from_utf8(bytes) {
        Ok(s) => Ok((s, "utf-8")),
        Err(e) => {
            // A size-limited sample may cut a multi-byte character in half at
            // the very end; that's still UTF-8.
            let incomplete_tail = e.utf8_error().error_len().is_none();
            let valid = e.utf8_error().valid_up_to();
            let bytes = e.into_bytes();
            if limit.is_some() && incomplete_tail {
                return Ok((String::from_utf8_lossy(&bytes[..valid]).into_owned(), "utf-8"));
            }
            Ok((bytes.iter().map(|&b| b as char).collect(), "latin-1"))
        }
    }
}

fn decode(bytes: Vec<u8>, encoding: &str) -> String {
    if encoding == "latin-1" {
        bytes.iter().map(|&b| b as char).collect()
    } else {
        String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }
}

fn detect_delimiter(sample: &str) -> u8 {
    let lines: Vec<&str> = sample.lines().filter(|l| !l.trim().is_empty()).take(20).collect();
    let mut best = (b',', 0usize, false);
    for d in [b',', b';', b'\t', b'|'] {
        let counts: Vec<usize> = lines.iter().map(|l| count_outside_quotes(l, d)).collect();
        let first = counts.first().copied().unwrap_or(0);
        let consistent = first > 0 && counts.iter().all(|&c| c == first);
        let better = match (consistent, best.2) {
            (true, false) => true,
            (a, b) if a == b => first > best.1,
            _ => false,
        };
        if first > 0 && better {
            best = (d, first, consistent);
        }
    }
    best.0
}

fn count_outside_quotes(line: &str, d: u8) -> usize {
    let mut n = 0;
    let mut quoted = false;
    for b in line.bytes() {
        if b == b'"' {
            quoted = !quoted;
        } else if b == d && !quoted {
            n += 1;
        }
    }
    n
}

fn reader(text: &str, delimiter: u8) -> csv::Reader<&[u8]> {
    csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes())
}

fn delimiter_byte(d: &str) -> AppResult<u8> {
    match d {
        "\\t" | "\t" | "tab" => Ok(b'\t'),
        s if s.len() == 1 && s.is_ascii() => Ok(s.as_bytes()[0]),
        _ => Err(AppError::invalid("The delimiter must be a single character")),
    }
}

fn delimiter_name(d: u8) -> String {
    if d == b'\t' {
        "\\t".into()
    } else {
        (d as char).to_string()
    }
}

// ------------------------------------------------------------- type sniffing

fn is_int(v: &str) -> bool {
    let v = v.strip_prefix('-').unwrap_or(v);
    !v.is_empty() && v.len() < 19 && v.bytes().all(|b| b.is_ascii_digit()) && !(v.len() > 1 && v.starts_with('0'))
}
fn is_decimal(v: &str) -> bool {
    !v.is_empty() && v.parse::<f64>().map(|f| f.is_finite()).unwrap_or(false) && !v.contains(['e', 'E', 'i', 'n', 'N', 'I'])
}
fn is_bool(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "true" | "false")
}
fn is_date(v: &str) -> bool {
    chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").is_ok()
}
fn is_timestamp(v: &str) -> bool {
    ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M"]
        .iter()
        .any(|f| chrono::NaiveDateTime::parse_from_str(v.trim_end_matches('Z'), f).is_ok())
}

fn sniff(values: &[&str]) -> &'static str {
    let vals: Vec<&str> = values.iter().map(|v| v.trim()).filter(|v| !v.is_empty()).collect();
    if vals.is_empty() {
        return "text";
    }
    let all = |f: fn(&str) -> bool| vals.iter().all(|v| f(v));
    if all(is_int) {
        "integer"
    } else if all(is_decimal) {
        "decimal"
    } else if all(is_bool) {
        "boolean"
    } else if all(is_date) {
        "date"
    } else if all(is_timestamp) {
        "timestamp"
    } else {
        "text"
    }
}

/// SQL type for a sniffed kind on a given engine.
pub fn sql_type(engine: Engine, kind: &str) -> String {
    let t = match (engine, kind) {
        (Engine::Sqlite, "integer") => "INTEGER",
        (Engine::Sqlite, "decimal") => "REAL",
        (Engine::Sqlite, _) => "TEXT",
        (Engine::Mysql, "integer") => "BIGINT",
        (Engine::Mysql, "decimal") => "DOUBLE",
        (Engine::Mysql, "boolean") => "BOOLEAN",
        (Engine::Mysql, "date") => "DATE",
        (Engine::Mysql, "timestamp") => "DATETIME(6)",
        (Engine::Mysql, _) => "TEXT",
        (Engine::Mssql, "integer") => "BIGINT",
        (Engine::Mssql, "decimal") => "FLOAT",
        (Engine::Mssql, "boolean") => "BIT",
        (Engine::Mssql, "date") => "DATE",
        (Engine::Mssql, "timestamp") => "DATETIME2",
        (Engine::Mssql, _) => "NVARCHAR(MAX)",
        (Engine::Duckdb, "integer") => "BIGINT",
        (Engine::Duckdb, "decimal") => "DOUBLE",
        (Engine::Duckdb, "boolean") => "BOOLEAN",
        (Engine::Duckdb, "date") => "DATE",
        (Engine::Duckdb, "timestamp") => "TIMESTAMP",
        (Engine::Duckdb, _) => "VARCHAR",
        (_, "integer") => "bigint",
        (_, "decimal") => "double precision",
        (_, "boolean") => "boolean",
        (_, "date") => "date",
        (_, "timestamp") => "timestamp",
        _ => "text",
    };
    t.to_string()
}

fn clean_name(raw: &str, i: usize, seen: &mut Vec<String>) -> String {
    let mut name = raw.trim().to_string();
    if name.is_empty() {
        name = format!("column_{}", i + 1);
    }
    let base = name.clone();
    let mut n = 2;
    while seen.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
        name = format!("{base}_{n}");
        n += 1;
    }
    seen.push(name.clone());
    name
}

pub fn preview(path: &str, delimiter: Option<&str>, has_header: Option<bool>) -> AppResult<CsvPreview> {
    let p = crate::drivers::sqlite::expand_path(path);
    let meta = std::fs::metadata(&p).map_err(|e| AppError::invalid(format!("Couldn't open {}: {e}", p.display())))?;
    let (sample, encoding) = read_text(&p, Some(4 * 1024 * 1024))?;
    let d = match delimiter.filter(|d| !d.is_empty()) {
        Some(d) => delimiter_byte(d)?,
        None => detect_delimiter(&sample),
    };
    let mut records: Vec<Vec<String>> = Vec::new();
    for rec in reader(&sample, d).records().take(SAMPLE_ROWS + 1) {
        match rec {
            Ok(r) => records.push(r.iter().map(str::to_string).collect()),
            Err(_) => break, // the 4 MB sample can end mid-record
        }
    }
    if records.is_empty() {
        return Err(AppError::invalid("The file is empty"));
    }
    let width = records.iter().map(|r| r.len()).max().unwrap_or(0);
    // Header guess: first row has no numbers while the second row does, or
    // every first-row cell is a non-empty, non-numeric label.
    let header = has_header.unwrap_or_else(|| {
        let first = &records[0];
        first.iter().all(|c| !c.trim().is_empty() && !is_decimal(c.trim()))
            && first.iter().collect::<std::collections::HashSet<_>>().len() == first.len()
    });
    let mut seen = Vec::new();
    let names: Vec<String> = (0..width)
        .map(|i| {
            let raw = if header { records[0].get(i).cloned().unwrap_or_default() } else { String::new() };
            clean_name(&raw, i, &mut seen)
        })
        .collect();
    let data = if header { &records[1..] } else { &records[..] };
    let columns = names
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let vals: Vec<&str> = data.iter().filter_map(|r| r.get(i).map(String::as_str)).collect();
            CsvColumn { name, kind: sniff(&vals).into() }
        })
        .collect();
    // Exact row count for files up to 200 MB; estimate beyond that.
    let total_rows = if meta.len() <= 200 * 1024 * 1024 {
        let (text, _) = read_text(&p, None)?;
        let n = reader(&text, d).records().count() as u64;
        n.saturating_sub(header as u64)
    } else {
        let avg = sample.len() as f64 / records.len().max(1) as f64;
        (meta.len() as f64 / avg.max(1.0)) as u64
    };
    Ok(CsvPreview {
        path: p.to_string_lossy().into_owned(),
        delimiter: delimiter_name(d),
        has_header: header,
        encoding: encoding.into(),
        columns,
        rows: data.iter().take(PREVIEW_ROWS).cloned().collect(),
        total_rows,
        file_size: meta.len(),
    })
}

// ------------------------------------------------------------------ running

fn literal(engine: Engine, v: &str, sql_type: &str) -> String {
    let t = sql_type.to_ascii_lowercase();
    // MySQL/SQL Server booleans are numbers.
    if matches!(engine, Engine::Mysql | Engine::Mssql) && (t == "bit" || t.starts_with("bool") || t == "tinyint(1)") {
        match v.trim().to_ascii_lowercase().as_str() {
            "true" | "t" | "yes" | "y" => return "1".into(),
            "false" | "f" | "no" | "n" => return "0".into(),
            _ => {}
        }
    }
    quote_literal(engine, v)
}

pub async fn run(session: &Session, req: ImportRequest, max_rows: Option<u64>) -> AppResult<u64> {
    let engine = session.engine();
    if req.columns.is_empty() {
        return Err(AppError::invalid("Choose at least one column to import"));
    }
    if req.table.trim().is_empty() {
        return Err(AppError::invalid("Choose a table"));
    }
    let d = delimiter_byte(&req.delimiter)?;
    let path = crate::drivers::sqlite::expand_path(&req.path);
    let raw = std::fs::read(&path).map_err(|e| AppError::invalid(format!("Couldn't open {}: {e}", path.display())))?;
    let mut raw = raw;
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        raw.drain(..3);
    }
    let text = decode(raw, if req.encoding.is_empty() { "utf-8" } else { &req.encoding });
    let total = (reader(&text, d).records().count() as u64).saturating_sub(req.has_header as u64);
    if let Some(max) = max_rows {
        if total > max {
            return Err(AppError::license(format!(
                "The free plan imports up to {max} rows at a time; this file has {total}. Upgrade to Rowglow Pro for unlimited imports."
            ))
            .with_code("limit_import"));
        }
    }

    let job = Arc::new(ImportJob::default());
    job.total.store(total, Ordering::Relaxed);
    jobs().lock().map_err(|_| AppError::internal("import lock"))?.insert(req.job_id.clone(), job.clone());
    let result = run_inner(session, engine, &req, &text, d, &job).await;
    if let Ok(mut m) = jobs().lock() {
        m.remove(&req.job_id);
    }
    result
}

async fn run_inner(session: &Session, engine: Engine, req: &ImportRequest, text: &str, d: u8, job: &ImportJob) -> AppResult<u64> {
    // Server databases: a separate connection so the import's transaction
    // never mixes with the editor. File databases: the editor connection
    // (DuckDB allows one connection set per file and process).
    let own;
    let (conn, ch): (&Session, Channel) = if engine.is_file() {
        (session, Channel::Editor)
    } else {
        own = session.fresh().await?;
        (&own, Channel::Editor)
    };
    let target = qualified(engine, &req.schema, &req.table);
    let cols = req
        .columns
        .iter()
        .map(|c| quote_ident(engine, &c.target))
        .collect::<Vec<_>>()
        .join(", ");
    let begin = match engine {
        Engine::Mssql => "BEGIN TRANSACTION",
        Engine::Mysql => "START TRANSACTION",
        _ => "BEGIN",
    };
    let exec = |sql: String| async move { conn.execute(ch, &sql, 1).await };
    let create_sql = || {
        let defs = req
            .columns
            .iter()
            .map(|c| {
                let t = if c.sql_type.trim().is_empty() { sql_type(engine, "text") } else { c.sql_type.trim().to_string() };
                format!("{} {t}", quote_ident(engine, &c.target))
            })
            .collect::<Vec<_>>()
            .join(",\n  ");
        format!("CREATE TABLE {target} (\n  {defs}\n)")
    };
    // MySQL commits implicitly on DDL, so its table is created first and
    // dropped again if the import fails.
    let mysql_created = engine == Engine::Mysql && req.create;
    if mysql_created {
        exec(create_sql()).await?;
    }
    if let Err(e) = exec(begin.into()).await {
        if mysql_created {
            let _ = exec(format!("DROP TABLE {target}")).await;
        }
        return Err(e);
    }
    let outcome: AppResult<u64> = async {
        if req.create && !mysql_created {
            exec(create_sql()).await?;
        }
        let mut rows = reader(text, d).into_records();
        if req.has_header {
            rows.next();
        }
        let mut done: u64 = 0;
        let mut batch: Vec<String> = Vec::with_capacity(BATCH);
        let mut batch_start = 1u64;
        loop {
            let next = rows.next();
            if let Some(rec) = &next {
                let rec = rec.as_ref().map_err(|e| AppError::invalid(format!("Row {}: {e}", done + batch.len() as u64 + 1)))?;
                let vals = req
                    .columns
                    .iter()
                    .map(|c| match rec.get(c.source) {
                        Some(v) if !(req.empty_as_null && v.is_empty()) => literal(engine, v, &c.sql_type),
                        _ => "NULL".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                batch.push(format!("({vals})"));
            }
            if batch.len() >= BATCH || (next.is_none() && !batch.is_empty()) {
                if job.cancel.load(Ordering::Relaxed) {
                    return Err(AppError::new("cancelled", "Import cancelled. Nothing was imported."));
                }
                let n = batch.len() as u64;
                let sql = format!("INSERT INTO {target} ({cols}) VALUES\n{}", batch.join(",\n"));
                if let Err(mut e) = exec(sql).await {
                    e.message = format!(
                        "Rows {}–{}: {}. Nothing was imported.",
                        batch_start,
                        batch_start + n - 1,
                        e.message
                    );
                    return Err(e);
                }
                done += n;
                batch_start += n;
                batch.clear();
                job.rows_done.store(done, Ordering::Relaxed);
            }
            if next.is_none() {
                break;
            }
        }
        Ok(done)
    }
    .await;
    match outcome {
        Ok(n) => {
            exec("COMMIT".into()).await?;
            Ok(n)
        }
        Err(e) => {
            let _ = exec(match engine {
                Engine::Mssql => "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION".into(),
                _ => "ROLLBACK".into(),
            })
            .await;
            if mysql_created {
                let _ = exec(format!("DROP TABLE {target}")).await;
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_and_sniffs() {
        let dir = std::env::temp_dir().join(format!("rg-csv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("people.csv");
        std::fs::write(
            &f,
            "\u{feff}id;name;score;active;born;seen\n1;Zoë;1.5;true;1990-01-02;2024-05-01 10:00:00\n2;\"Bob; Jr\";2;false;;2024-05-01T11:30:00\n3;;3.25;true;2001-12-31;\n",
        )
        .unwrap();
        let p = preview(f.to_str().unwrap(), None, None).unwrap();
        assert_eq!(p.delimiter, ";");
        assert!(p.has_header);
        assert_eq!(p.total_rows, 3);
        let kinds: Vec<&str> = p.columns.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(kinds, ["integer", "text", "decimal", "boolean", "date", "timestamp"]);
        assert_eq!(p.columns[0].name, "id");
        assert_eq!(p.rows[1][1], "Bob; Jr");
        assert_eq!(p.encoding, "utf-8");

        let latin = dir.join("latin.csv");
        std::fs::write(&latin, b"a,b\n1,caf\xe9\n").unwrap();
        let p = preview(latin.to_str().unwrap(), None, None).unwrap();
        assert_eq!(p.encoding, "latin-1");
        assert_eq!(p.rows[0][1], "café");

        let nohead = dir.join("nohead.tsv");
        std::fs::write(&nohead, "1\t2\n3\t4\n").unwrap();
        let p = preview(nohead.to_str().unwrap(), None, None).unwrap();
        assert_eq!(p.delimiter, "\\t");
        assert!(!p.has_header);
        assert_eq!(p.columns[1].name, "column_2");
        assert_eq!(p.total_rows, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn literals() {
        assert_eq!(literal(Engine::Mssql, "true", "BIT"), "1");
        assert_eq!(literal(Engine::Mysql, "no", "boolean"), "0");
        assert_eq!(literal(Engine::Postgres, "it's", "text"), "'it''s'");
        assert_eq!(literal(Engine::Mssql, "Zoë", "nvarchar(max)"), "N'Zoë'");
        assert_eq!(sql_type(Engine::Sqlite, "date"), "TEXT");
        assert!(!is_int("007"));
        assert!(is_int("-42"));
    }
}
