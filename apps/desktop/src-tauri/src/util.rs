use crate::model::Engine;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

pub fn quote_ident(engine: Engine, ident: &str) -> String {
    match engine {
        Engine::Mysql => format!("`{}`", ident.replace('`', "``")),
        Engine::Mssql => format!("[{}]", ident.replace(']', "]]")),
        _ => format!("\"{}\"", ident.replace('"', "\"\"")),
    }
}

pub fn quote_literal(engine: Engine, value: &str) -> String {
    match engine {
        Engine::Mysql => {
            let mut out = String::with_capacity(value.len() + 2);
            out.push('\'');
            for ch in value.chars() {
                match ch {
                    '\\' => out.push_str("\\\\"),
                    '\'' => out.push_str("''"),
                    '\0' => out.push_str("\\0"),
                    _ => out.push(ch),
                }
            }
            out.push('\'');
            out
        }
        // N'…' keeps non-ASCII text intact in nvarchar columns.
        Engine::Mssql => format!("N'{}'", value.replace('\'', "''")),
        _ => format!("'{}'", value.replace('\'', "''")),
    }
}

/// Qualified name: "schema"."table" (SQLite main schema is omitted).
pub fn qualified(engine: Engine, schema: &str, name: &str) -> String {
    if schema.is_empty() || (engine == Engine::Sqlite && schema == "main") {
        quote_ident(engine, name)
    } else {
        format!("{}.{}", quote_ident(engine, schema), quote_ident(engine, name))
    }
}

/// Very small classifier used for history and the read-only guard.
pub fn first_keyword(sql: &str) -> String {
    let mut s = sql.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.split_once('\n').map(|(_, r)| r).unwrap_or("").trim_start();
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.split_once("*/").map(|(_, r)| r).unwrap_or("").trim_start();
        } else {
            break;
        }
    }
    s.trim_start_matches('(')
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

/// Login name of the current user (Postgres' default role name).
/// `USER` on macOS/Linux, `LOGNAME` when a desktop session doesn't set `USER`,
/// `USERNAME` on Windows.
pub fn os_user() -> String {
    ["USER", "LOGNAME", "USERNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| "postgres".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote_ident(Engine::Postgres, "a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_ident(Engine::Mysql, "a`b"), "`a``b`");
        assert_eq!(quote_literal(Engine::Postgres, "O'Brien\\"), "'O''Brien\\'");
        assert_eq!(quote_literal(Engine::Mysql, "O'Brien\\"), "'O''Brien\\\\'");
        assert_eq!(qualified(Engine::Sqlite, "main", "t"), "\"t\"");
        assert_eq!(qualified(Engine::Postgres, "public", "t"), "\"public\".\"t\"");
        assert_eq!(qualified(Engine::Mssql, "dbo", "a]b"), "[dbo].[a]]b]");
        assert_eq!(quote_literal(Engine::Mssql, "O'Brien"), "N'O''Brien'");
    }

    #[test]
    fn keywords() {
        assert_eq!(first_keyword("  -- hi\n /* x */ select 1"), "SELECT");
        assert_eq!(first_keyword("(SELECT 1)"), "SELECT");
        assert_eq!(first_keyword("with x as (select 1) select * from x"), "WITH");
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
    }
}
