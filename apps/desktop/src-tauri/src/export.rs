//! Streams a whole table / query to CSV, JSON or SQL INSERT statements in
//! pages of 5 000 rows so memory stays flat for big tables.

use crate::drivers::{Channel, Session};
use crate::error::{AppError, AppResult};
use crate::model::{Column, Engine};
use crate::util::{qualified, quote_ident, quote_literal};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

const PAGE: usize = 5_000;

pub fn is_numeric_type(t: &str) -> bool {
    let t = t.to_lowercase();
    ["int", "numeric", "decimal", "float", "double", "real", "serial", "year"]
        .iter()
        .any(|k| t.contains(k))
        && !t.contains("interval")
        && !t.contains("point")
}

fn is_bool_type(t: &str) -> bool {
    matches!(t.to_lowercase().as_str(), "bool" | "boolean")
}

fn is_json_type(t: &str) -> bool {
    matches!(t.to_lowercase().as_str(), "json" | "jsonb")
}

fn json_value(v: &Option<String>, col: &Column) -> serde_json::Value {
    match v {
        None => serde_json::Value::Null,
        Some(s) => {
            if is_numeric_type(&col.type_name) {
                if let Ok(n) = s.parse::<i64>() {
                    return n.into();
                }
                if let Ok(f) = s.parse::<f64>() {
                    if let Some(n) = serde_json::Number::from_f64(f) {
                        return serde_json::Value::Number(n);
                    }
                }
            }
            if is_bool_type(&col.type_name) {
                match s.as_str() {
                    "t" | "true" | "1" => return true.into(),
                    "f" | "false" | "0" => return false.into(),
                    _ => {}
                }
            }
            if is_json_type(&col.type_name) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
                    return v;
                }
            }
            serde_json::Value::String(s.clone())
        }
    }
}

pub fn sql_value(engine: Engine, v: &Option<String>, col: &Column) -> String {
    match v {
        None => "NULL".into(),
        Some(s) => {
            if is_numeric_type(&col.type_name) && s.parse::<f64>().is_ok() {
                s.clone()
            } else {
                quote_literal(engine, s)
            }
        }
    }
}

/// True if `sql` has an ORDER BY outside any parentheses (comments and
/// strings ignored).
fn has_top_level_order_by(sql: &str) -> bool {
    let clean = crate::safety::strip(sql).to_ascii_uppercase();
    let mut depth = 0i32;
    let mut top = String::with_capacity(clean.len());
    for ch in clean.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ if depth == 0 => top.push(ch),
            _ => {}
        }
    }
    let words: Vec<&str> = top.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).collect();
    words.windows(2).any(|w| w[0] == "ORDER" && w[1] == "BY")
}

/// One page of `base`, in the engine's pagination syntax.
pub fn page_sql(engine: Engine, base: &str, limit: usize, offset: usize) -> String {
    match engine {
        Engine::Mssql => {
            // New line first so a trailing `-- comment` can't swallow the clause.
            let order = if has_top_level_order_by(base) { "" } else { "ORDER BY (SELECT NULL) " };
            format!("{base}\n{order}OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
        }
        _ => format!("{base}\nLIMIT {limit} OFFSET {offset}"),
    }
}

pub async fn export(
    session: &Session,
    sql: &str,
    path: &Path,
    format: &str,
    schema: &str,
    table: &str,
) -> AppResult<u64> {
    let engine = session.engine();
    let base = sql.trim().trim_end_matches(';').to_string();
    let file = File::create(path)?;
    let mut w = BufWriter::new(file);
    let mut offset = 0usize;
    let mut total: u64 = 0;
    let mut columns: Vec<Column> = Vec::new();
    let mut wrote_json_row = false;
    let target = if table.is_empty() {
        quote_ident(engine, "export")
    } else {
        qualified(engine, schema, table)
    };
    if format == "json" {
        w.write_all(b"[\n")?;
    }
    loop {
        let page_sql = page_sql(engine, &base, PAGE, offset);
        let r = session.execute(Channel::Aux, &page_sql, PAGE).await?;
        let first_page = columns.is_empty();
        if first_page {
            columns = r.columns.clone();
        }
        let n = r.rows.len();
        match format {
            "csv" => {
                let mut cw = csv::Writer::from_writer(Vec::new());
                if first_page {
                    cw.write_record(columns.iter().map(|c| c.name.as_str()))?;
                }
                for row in &r.rows {
                    cw.write_record(row.iter().map(|v| v.as_deref().unwrap_or("")))?;
                }
                let bytes = cw.into_inner().map_err(|e| AppError::internal(e.to_string()))?;
                w.write_all(&bytes)?;
            }
            "json" => {
                for row in &r.rows {
                    let mut obj = serde_json::Map::new();
                    for (i, c) in columns.iter().enumerate() {
                        obj.insert(c.name.clone(), json_value(&row[i], c));
                    }
                    let prefix = if wrote_json_row { ",\n  " } else { "  " };
                    wrote_json_row = true;
                    w.write_all(prefix.as_bytes())?;
                    w.write_all(serde_json::to_string(&obj)?.as_bytes())?;
                }
            }
            "sql" => {
                let cols = columns
                    .iter()
                    .map(|c| quote_ident(engine, &c.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                for row in &r.rows {
                    let vals = row
                        .iter()
                        .zip(columns.iter())
                        .map(|(v, c)| sql_value(engine, v, c))
                        .collect::<Vec<_>>()
                        .join(", ");
                    writeln!(w, "INSERT INTO {target} ({cols}) VALUES ({vals});")?;
                }
            }
            _ => return Err(AppError::invalid(format!("Unknown export format {format}"))),
        }
        total += n as u64;
        offset += n;
        if n < PAGE {
            break;
        }
    }
    if format == "json" {
        w.write_all(b"\n]\n")?;
    }
    w.flush()?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_detection() {
        assert!(is_numeric_type("int4"));
        assert!(is_numeric_type("numeric(10,2)"));
        assert!(is_numeric_type("bigint unsigned"));
        assert!(!is_numeric_type("interval"));
        assert!(!is_numeric_type("text"));
        let col = Column { name: "a".into(), type_name: "int8".into() };
        assert_eq!(sql_value(Engine::Postgres, &Some("5".into()), &col), "5");
        assert_eq!(sql_value(Engine::Postgres, &None, &col), "NULL");
        let tcol = Column { name: "a".into(), type_name: "text".into() };
        assert_eq!(sql_value(Engine::Mysql, &Some("it's".into()), &tcol), "'it''s'");
        assert_eq!(json_value(&Some("12".into()), &col), serde_json::json!(12));
        assert_eq!(
            page_sql(Engine::Mssql, "SELECT * FROM [t] ORDER BY [id]", 10, 20),
            "SELECT * FROM [t] ORDER BY [id]\nOFFSET 20 ROWS FETCH NEXT 10 ROWS ONLY"
        );
        assert_eq!(
            page_sql(Engine::Mssql, "SELECT *, ROW_NUMBER() OVER (ORDER BY a) FROM t -- order by", 5, 0),
            "SELECT *, ROW_NUMBER() OVER (ORDER BY a) FROM t -- order by\nORDER BY (SELECT NULL) OFFSET 0 ROWS FETCH NEXT 5 ROWS ONLY"
        );
        assert_eq!(page_sql(Engine::Duckdb, "SELECT 1", 5, 10), "SELECT 1\nLIMIT 5 OFFSET 10");
    }
}
