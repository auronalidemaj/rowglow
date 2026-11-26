//! Statement classification used to enforce read-only connections in the core
//! (defence in depth on top of the database's own read-only session setting).

/// Removes comments and the contents of quoted strings / identifiers /
/// dollar-quoted bodies so keyword checks only see real SQL.
pub fn strip(sql: &str) -> String {
    let b: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let n = b.get(i + 1).copied();
        if c == '-' && n == Some('-') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            out.push(' ');
            continue;
        }
        if c == '/' && n == Some('*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            out.push(' ');
            continue;
        }
        if c == '\'' || c == '"' || c == '`' {
            let q = c;
            i += 1;
            while i < b.len() {
                if b[i] == '\\' && q != '"' {
                    i += 2;
                    continue;
                }
                if b[i] == q {
                    if b.get(i + 1) == Some(&q) {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            out.push_str(if q == '\'' { "''" } else { "x" });
            continue;
        }
        if c == '$' {
            // $tag$ ... $tag$
            let mut j = i + 1;
            while j < b.len() && (b[j].is_alphanumeric() || b[j] == '_') {
                j += 1;
            }
            if j < b.len() && b[j] == '$' {
                let tag: String = b[i..=j].iter().collect();
                let rest: String = b[j + 1..].iter().collect();
                if let Some(pos) = rest.find(&tag) {
                    i = j + 1 + rest[..pos].chars().count() + tag.chars().count();
                    out.push_str("''");
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// T-SQL flavour of [`strip`]: no backslash escapes, nested block comments,
/// and [bracketed] identifiers.
pub fn strip_tsql(sql: &str) -> String {
    let b: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let n = b.get(i + 1).copied();
        if c == '-' && n == Some('-') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            out.push(' ');
            continue;
        }
        if c == '/' && n == Some('*') {
            let mut depth = 1;
            i += 2;
            while i < b.len() && depth > 0 {
                if b[i] == '*' && b.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else if b[i] == '/' && b.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            out.push(' ');
            continue;
        }
        if c == '\'' || c == '"' || c == '[' {
            let close = if c == '[' { ']' } else { c };
            i += 1;
            while i < b.len() {
                if b[i] == close {
                    if b.get(i + 1) == Some(&close) {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            out.push_str(if c == '\'' { " '' " } else { " x " });
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Words that can change data, schema, permissions or server state in T-SQL.
const TSQL_WRITES: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "MERGE", "TRUNCATE", "DROP", "CREATE", "ALTER", "INTO", "EXEC", "EXECUTE", "GRANT",
    "REVOKE", "DENY", "BULK", "BACKUP", "RESTORE", "DBCC", "KILL", "SHUTDOWN", "RECONFIGURE", "WRITETEXT", "UPDATETEXT",
    "OPENROWSET", "OPENQUERY", "OPENDATASOURCE", "ENABLE", "DISABLE", "RENAME", "SEND", "RECEIVE", "MOVE", "CHECKPOINT",
];

/// Read-only guard for a statement on a given engine. SQL Server has no
/// session-level read-only mode and runs several statements in one batch
/// without semicolons, so any write keyword anywhere counts there.
pub fn is_write_for(engine: crate::model::Engine, sql: &str) -> bool {
    if engine == crate::model::Engine::Mssql {
        let w = words(&strip_tsql(sql));
        return w.iter().any(|x| {
            TSQL_WRITES.contains(&x.as_str()) || x.starts_with("SP_") || x.starts_with("XP_")
        }) || (w.first().map(|f| f == "BEGIN").unwrap_or(false)
            && !matches!(w.get(1).map(|s| s.as_str()), Some("TRAN" | "TRANSACTION")));
    }
    is_write(sql)
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_uppercase())
        .collect()
}

/// True if the statement may modify data, schema or session safety settings.
pub fn is_write(sql: &str) -> bool {
    let clean = strip(sql);
    let w = words(&clean);
    let Some(first) = w.first().map(|s| s.as_str()) else {
        return false;
    };
    let has = |k: &str| w.iter().any(|x| x == k);
    let dml = has("INSERT") || has("UPDATE") || has("DELETE") || has("MERGE") || has("UPSERT");
    let read_write_toggle = (has("READ") && has("WRITE"))
        || w.iter().any(|x| x == "DEFAULT_TRANSACTION_READ_ONLY" || x == "TRANSACTION_READ_ONLY" || x == "READ_ONLY");
    match first {
        "SELECT" => has("INTO") && !clean.to_ascii_uppercase().contains("INTO @"),
        "WITH" => dml || has("INTO"),
        "EXPLAIN" => (has("ANALYZE") || has("ANALYSE")) && dml,
        "SHOW" | "DESCRIBE" | "DESC" | "VALUES" | "TABLE" | "USE" | "PRAGMA" | "FETCH" | "CLOSE" | "HELP" | "SUMMARIZE" => false,
        // DuckDB's FROM-first SELECT
        "FROM" => has("INSERT") || has("UPDATE") || has("DELETE") || has("COPY"),
        "BEGIN" | "START" | "COMMIT" | "ROLLBACK" | "END" | "SAVEPOINT" | "RELEASE" | "ABORT" => read_write_toggle,
        "SET" | "RESET" => read_write_toggle,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies() {
        assert!(!is_write("select * from t"));
        assert!(!is_write("  -- update\n SELECT 'delete from x' FROM t"));
        assert!(!is_write("with x as (select 1) select * from x"));
        assert!(is_write("with x as (delete from t returning *) select * from x"));
        assert!(is_write("UPDATE t SET a = 1"));
        assert!(is_write("drop table t"));
        assert!(is_write("select * into new_t from t"));
        assert!(!is_write("select 1 into @x"));
        assert!(!is_write("explain select 1"));
        assert!(is_write("explain analyze delete from t"));
        assert!(!is_write("SET search_path TO public"));
        assert!(is_write("SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE"));
        assert!(is_write("set default_transaction_read_only = off"));
        assert!(is_write("START TRANSACTION READ WRITE"));
        assert!(!is_write("BEGIN"));
        assert!(is_write("CREATE TABLE x (id int)"));
        assert!(is_write("do $$ begin delete from t; end $$"));
        assert!(!is_write("select $$update$$"));
        assert!(is_write("CALL p()"));
        assert!(!is_write("FROM tbl LIMIT 5"));
        assert!(is_write("EXEC dbo.purge"));
        assert!(!is_write("SELECT * FROM [order] WHERE [delete] = 1"));

        use crate::model::Engine::Mssql;
        assert!(!is_write_for(Mssql, "SELECT * FROM [order] WHERE [delete] = 1 AND note = 'drop it'"));
        assert!(is_write_for(Mssql, "SELECT 1\nDELETE FROM t"));
        assert!(is_write_for(Mssql, "SELECT 'C:\\' DELETE FROM t"));
        assert!(is_write_for(Mssql, "BEGIN UPDATE t SET a = 1 END"));
        assert!(!is_write_for(Mssql, "BEGIN TRAN"));
        assert!(is_write_for(Mssql, "EXEC sp_who"));
        assert!(is_write_for(Mssql, "SELECT * INTO t2 FROM t"));
        assert!(!is_write_for(Mssql, "/* outer /* nested delete */ still comment */ SELECT 1"));
        assert!(!is_write_for(Mssql, "SET NOCOUNT ON; SELECT TOP 10 * FROM sys.objects"));
    }
}
