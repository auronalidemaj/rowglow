//! Local application database (`app.db`): saved connections, query history,
//! snippets and key/value settings.

use crate::error::{AppError, AppResult};
use crate::model::{ConnectionConfig, Engine, HistoryEntry, Snippet, SslMode};
use crate::util::{new_id, now_secs};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS connections (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    engine TEXT NOT NULL,
    host TEXT NOT NULL DEFAULT '',
    port INTEGER NOT NULL DEFAULT 0,
    username TEXT NOT NULL DEFAULT '',
    database TEXT NOT NULL DEFAULT '',
    ssl_mode TEXT NOT NULL DEFAULT 'prefer',
    file_path TEXT NOT NULL DEFAULT '',
    color TEXT NOT NULL DEFAULT '',
    env TEXT NOT NULL DEFAULT 'local',
    read_only INTEGER NOT NULL DEFAULT 0,
    group_name TEXT NOT NULL DEFAULT '',
    sort INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_used_at INTEGER
);
CREATE TABLE IF NOT EXISTS history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    connection_id TEXT NOT NULL,
    database TEXT NOT NULL DEFAULT '',
    sql TEXT NOT NULL,
    duration_ms REAL NOT NULL DEFAULT 0,
    rows INTEGER,
    error TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS history_conn_idx ON history(connection_id, id DESC);
CREATE TABLE IF NOT EXISTS snippets (
    id TEXT PRIMARY KEY,
    connection_id TEXT,
    name TEXT NOT NULL,
    sql TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS secrets (
    id TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ssh_known_hosts (
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    fingerprint TEXT NOT NULL,
    added_at INTEGER NOT NULL,
    PRIMARY KEY (host, port)
);
"#;

/// Columns added after 1.0, applied to existing app.db files on open.
const MIGRATIONS: &[&str] = &[
    "ALTER TABLE connections ADD COLUMN ssh_enabled INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE connections ADD COLUMN ssh_host TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE connections ADD COLUMN ssh_port INTEGER NOT NULL DEFAULT 22",
    "ALTER TABLE connections ADD COLUMN ssh_user TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE connections ADD COLUMN ssh_auth TEXT NOT NULL DEFAULT 'password'",
    "ALTER TABLE connections ADD COLUMN ssh_key_path TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE connections ADD COLUMN options TEXT NOT NULL DEFAULT '{}'",
];

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    for sql in MIGRATIONS {
        if let Err(e) = conn.execute_batch(sql) {
            // "duplicate column name" = already migrated.
            if !e.to_string().contains("duplicate column") {
                return Err(e);
            }
        }
    }
    Ok(())
}

fn row_to_connection(r: &rusqlite::Row) -> rusqlite::Result<ConnectionConfig> {
    let engine: String = r.get("engine")?;
    let ssl: String = r.get("ssl_mode")?;
    Ok(ConnectionConfig {
        id: r.get("id")?,
        name: r.get("name")?,
        engine: Engine::parse(&engine).unwrap_or(Engine::Postgres),
        host: r.get("host")?,
        port: r.get::<_, i64>("port")? as u16,
        username: r.get("username")?,
        database: r.get("database")?,
        ssl_mode: SslMode::parse(&ssl),
        file_path: r.get("file_path")?,
        color: r.get("color")?,
        env: r.get("env")?,
        read_only: r.get::<_, i64>("read_only")? != 0,
        group_name: r.get("group_name")?,
        ssh_enabled: r.get::<_, i64>("ssh_enabled")? != 0,
        ssh_host: r.get("ssh_host")?,
        ssh_port: r.get::<_, i64>("ssh_port")? as u16,
        ssh_user: r.get("ssh_user")?,
        ssh_auth: r.get("ssh_auth")?,
        ssh_key_path: r.get("ssh_key_path")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        last_used_at: r.get("last_used_at")?,
        has_password: false,
        has_ssh_secret: false,
        options: r
            .get::<_, String>("options")
            .ok()
            .and_then(|o| serde_json::from_str(&o).ok())
            .unwrap_or_default(),
        sentinel_password: None,
    })
}

/// SQLite said the file is damaged or isn't a database at all.
fn is_corrupt_error(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if matches!(f.code, rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
    )
}

/// `PRAGMA quick_check` reports damage. Other errors (busy, I/O) don't count:
/// a file that's merely locked must never be moved aside.
fn quick_check_damaged(c: &Connection) -> bool {
    match c.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)) {
        Ok(s) => s != "ok",
        Err(e) => is_corrupt_error(&e),
    }
}

/// Whether the file at `path` exists and is damaged.
fn file_is_damaged(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    match Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => quick_check_damaged(&c),
        Err(e) => is_corrupt_error(&e),
    }
}

impl Store {
    /// Opens app.db. If the file is damaged, it's moved aside as
    /// `app.db.corrupt-<unix time>` (with its -wal/-shm files) and a fresh one
    /// is created; the moved file is returned so the user can be told. Any
    /// other failure (permissions, full disk, locked) is returned as an error
    /// and nothing is moved.
    pub fn open_or_recover(path: &Path) -> AppResult<(Store, Option<PathBuf>)> {
        match Store::open(path) {
            Ok(store) => {
                if !store.with(|c| Ok(quick_check_damaged(c))).unwrap_or(false) {
                    return Ok((store, None));
                }
                drop(store);
            }
            Err(e) => {
                if !file_is_damaged(path) {
                    return Err(e);
                }
            }
        }
        let backup = path.with_file_name(format!(
            "{}.corrupt-{}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("app.db"),
            now_secs()
        ));
        std::fs::rename(path, &backup)?;
        for ext in ["-wal", "-shm"] {
            let side = PathBuf::from(format!("{}{ext}", path.display()));
            if side.exists() {
                let _ = std::fs::rename(&side, format!("{}{ext}", backup.display()));
            }
        }
        Ok((Store::open(path)?, Some(backup)))
    }

    pub fn open(path: &Path) -> AppResult<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> AppResult<Store> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> AppResult<T> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| AppError::internal("app database lock poisoned"))?;
        f(&guard).map_err(AppError::from)
    }

    // ---------- connections ----------

    pub fn list_connections(&self) -> AppResult<Vec<ConnectionConfig>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT * FROM connections ORDER BY group_name COLLATE NOCASE, sort, name COLLATE NOCASE",
            )?;
            let rows = stmt.query_map([], row_to_connection)?;
            rows.collect()
        })
    }

    pub fn get_connection(&self, id: &str) -> AppResult<ConnectionConfig> {
        self.with(|c| {
            c.query_row("SELECT * FROM connections WHERE id = ?1", [id], row_to_connection)
                .optional()
        })?
        .ok_or_else(|| AppError::not_found("Connection not found"))
    }

    pub fn count_connections(&self) -> AppResult<i64> {
        self.with(|c| c.query_row("SELECT count(*) FROM connections", [], |r| r.get(0)))
    }

    pub fn upsert_connection(&self, cfg: &mut ConnectionConfig) -> AppResult<()> {
        let now = now_secs();
        if cfg.id.is_empty() {
            cfg.id = new_id();
            cfg.created_at = now;
        }
        if cfg.created_at == 0 {
            cfg.created_at = now;
        }
        cfg.updated_at = now;
        let c = cfg.clone();
        self.with(|conn| {
            conn.execute(
                r#"INSERT INTO connections (id, name, engine, host, port, username, database, ssl_mode, file_path,
                       color, env, read_only, group_name, created_at, updated_at, last_used_at,
                       ssh_enabled, ssh_host, ssh_port, ssh_user, ssh_auth, ssh_key_path, options)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                           ?17, ?18, ?19, ?20, ?21, ?22, ?23)
                   ON CONFLICT(id) DO UPDATE SET name=excluded.name, engine=excluded.engine, host=excluded.host,
                       port=excluded.port, username=excluded.username, database=excluded.database,
                       ssl_mode=excluded.ssl_mode, file_path=excluded.file_path, color=excluded.color,
                       env=excluded.env, read_only=excluded.read_only, group_name=excluded.group_name,
                       updated_at=excluded.updated_at, ssh_enabled=excluded.ssh_enabled, ssh_host=excluded.ssh_host,
                       ssh_port=excluded.ssh_port, ssh_user=excluded.ssh_user, ssh_auth=excluded.ssh_auth,
                       ssh_key_path=excluded.ssh_key_path, options=excluded.options"#,
                params![
                    c.id,
                    c.name,
                    c.engine.as_str(),
                    c.host,
                    c.port as i64,
                    c.username,
                    c.database,
                    c.ssl_mode.as_str(),
                    c.file_path,
                    c.color,
                    if c.env.is_empty() { "local".to_string() } else { c.env.clone() },
                    c.read_only as i64,
                    c.group_name,
                    c.created_at,
                    c.updated_at,
                    c.last_used_at,
                    c.ssh_enabled as i64,
                    c.ssh_host,
                    if c.ssh_port == 0 { 22 } else { c.ssh_port as i64 },
                    c.ssh_user,
                    if c.ssh_auth.is_empty() { "password".to_string() } else { c.ssh_auth.clone() },
                    c.ssh_key_path,
                    serde_json::to_string(&c.options).unwrap_or_else(|_| "{}".into())
                ],
            )
            .map(|_| ())
        })
    }

    pub fn delete_connection(&self, id: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute("DELETE FROM connections WHERE id = ?1", [id])?;
            c.execute("DELETE FROM history WHERE connection_id = ?1", [id])?;
            c.execute("DELETE FROM snippets WHERE connection_id = ?1", [id])?;
            Ok(())
        })
    }

    pub fn touch_connection(&self, id: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE connections SET last_used_at = ?2 WHERE id = ?1",
                params![id, now_secs()],
            )
            .map(|_| ())
        })
    }

    // ---------- history ----------

    pub fn add_history(&self, e: &HistoryEntry, keep: Option<u32>) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO history (connection_id, database, sql, duration_ms, rows, error, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![e.connection_id, e.database, e.sql, e.duration_ms, e.rows, e.error, now_secs()],
            )?;
            if let Some(keep) = keep {
                c.execute(
                    "DELETE FROM history WHERE id NOT IN (SELECT id FROM history ORDER BY id DESC LIMIT ?1)",
                    [keep as i64],
                )?;
            }
            Ok(())
        })
    }

    pub fn list_history(
        &self,
        connection_id: Option<&str>,
        search: Option<&str>,
        limit: u32,
    ) -> AppResult<Vec<HistoryEntry>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, connection_id, database, sql, duration_ms, rows, error, created_at FROM history
                 WHERE (?1 IS NULL OR connection_id = ?1)
                   AND (?2 IS NULL OR sql LIKE '%' || ?2 || '%')
                 ORDER BY id DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![connection_id, search, limit as i64], |r| {
                Ok(HistoryEntry {
                    id: r.get(0)?,
                    connection_id: r.get(1)?,
                    database: r.get(2)?,
                    sql: r.get(3)?,
                    duration_ms: r.get(4)?,
                    rows: r.get(5)?,
                    error: r.get(6)?,
                    created_at: r.get(7)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn clear_history(&self, connection_id: Option<&str>) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "DELETE FROM history WHERE (?1 IS NULL OR connection_id = ?1)",
                params![connection_id],
            )
            .map(|_| ())
        })
    }

    // ---------- snippets ----------

    pub fn list_snippets(&self) -> AppResult<Vec<Snippet>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, connection_id, name, sql, created_at, updated_at FROM snippets ORDER BY name COLLATE NOCASE",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(Snippet {
                    id: r.get(0)?,
                    connection_id: r.get(1)?,
                    name: r.get(2)?,
                    sql: r.get(3)?,
                    created_at: r.get(4)?,
                    updated_at: r.get(5)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn count_snippets(&self) -> AppResult<i64> {
        self.with(|c| c.query_row("SELECT count(*) FROM snippets", [], |r| r.get(0)))
    }

    pub fn snippet_exists(&self, id: &str) -> AppResult<bool> {
        self.with(|c| {
            c.query_row("SELECT 1 FROM snippets WHERE id = ?1", [id], |_| Ok(()))
                .optional()
                .map(|o| o.is_some())
        })
    }

    pub fn save_snippet(&self, s: &mut Snippet) -> AppResult<()> {
        let now = now_secs();
        if s.id.is_empty() {
            s.id = new_id();
            s.created_at = now;
        }
        s.updated_at = now;
        let s = s.clone();
        self.with(|c| {
            c.execute(
                "INSERT INTO snippets (id, connection_id, name, sql, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET connection_id=excluded.connection_id, name=excluded.name,
                    sql=excluded.sql, updated_at=excluded.updated_at",
                params![s.id, s.connection_id, s.name, s.sql, s.created_at.max(1), s.updated_at],
            )
            .map(|_| ())
        })
    }

    pub fn delete_snippet(&self, id: &str) -> AppResult<()> {
        self.with(|c| c.execute("DELETE FROM snippets WHERE id = ?1", [id]).map(|_| ()))
    }

    // ---------- kv ----------

    pub fn kv_get(&self, key: &str) -> AppResult<Option<String>> {
        self.with(|c| {
            c.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
                .optional()
        })
    }

    pub fn kv_set(&self, key: &str, value: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
        })
    }

    pub fn kv_delete(&self, key: &str) -> AppResult<()> {
        self.with(|c| c.execute("DELETE FROM kv WHERE key = ?1", [key]).map(|_| ()))
    }

    // ---------- SSH known hosts (trust on first use) ----------

    pub fn known_host(&self, host: &str, port: u16) -> AppResult<Option<String>> {
        self.with(|c| {
            c.query_row(
                "SELECT fingerprint FROM ssh_known_hosts WHERE host = ?1 AND port = ?2",
                params![host, port as i64],
                |r| r.get(0),
            )
            .optional()
        })
    }

    pub fn trust_host(&self, host: &str, port: u16, fingerprint: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO ssh_known_hosts (host, port, fingerprint, added_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(host, port) DO UPDATE SET fingerprint = excluded.fingerprint, added_at = excluded.added_at",
                params![host, port as i64, fingerprint, now_secs()],
            )
            .map(|_| ())
        })
    }

    pub fn forget_host(&self, host: &str, port: u16) -> AppResult<()> {
        self.with(|c| {
            c.execute("DELETE FROM ssh_known_hosts WHERE host = ?1 AND port = ?2", params![host, port as i64])
                .map(|_| ())
        })
    }

    // ---------- secrets fallback (dev builds / keychain unavailable) ----------

    /// Secrets kept in app.db (the keychain fallback).
    pub fn secret_count(&self) -> AppResult<i64> {
        self.with(|c| c.query_row("SELECT COUNT(*) FROM secrets", [], |r| r.get(0)))
    }

    pub fn secret_get(&self, id: &str) -> AppResult<Option<String>> {
        self.with(|c| {
            c.query_row("SELECT value FROM secrets WHERE id = ?1", [id], |r| r.get(0))
                .optional()
        })
    }

    pub fn secret_set(&self, id: &str, value: &str) -> AppResult<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO secrets (id, value) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET value = excluded.value",
                params![id, value],
            )
            .map(|_| ())
        })
    }

    pub fn secret_delete(&self, id: &str) -> AppResult<()> {
        self.with(|c| c.execute("DELETE FROM secrets WHERE id = ?1", [id]).map(|_| ()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damaged_app_db_is_moved_aside() {
        let dir = std::env::temp_dir().join(format!("rowglow-store-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.db");

        // A healthy database is opened as is, never moved.
        let (store, moved) = Store::open_or_recover(&path).unwrap();
        store.kv_set("hello", "world").unwrap();
        drop(store);
        assert!(moved.is_none());
        let (store, moved) = Store::open_or_recover(&path).unwrap();
        assert!(moved.is_none());
        assert_eq!(store.kv_get("hello").unwrap().as_deref(), Some("world"));
        drop(store);

        // Garbage instead of a database: moved aside, fresh one created.
        for f in ["app.db-wal", "app.db-shm"] {
            let _ = std::fs::remove_file(dir.join(f));
        }
        std::fs::write(&path, vec![0x42u8; 8192]).unwrap();
        let (store, moved) = Store::open_or_recover(&path).unwrap();
        let moved = moved.expect("damaged file should be moved");
        assert!(moved.file_name().unwrap().to_string_lossy().starts_with("app.db.corrupt-"));
        assert_eq!(std::fs::read(&moved).unwrap(), vec![0x42u8; 8192]);
        assert!(store.list_connections().unwrap().is_empty());
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn connections_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let mut c = ConnectionConfig {
            id: String::new(),
            name: "Local".into(),
            engine: Engine::Postgres,
            host: "localhost".into(),
            port: 5432,
            username: "me".into(),
            database: "postgres".into(),
            ssl_mode: SslMode::Disable,
            file_path: String::new(),
            color: "#f00".into(),
            env: String::new(),
            read_only: true,
            group_name: String::new(),
            ssh_enabled: false,
            ssh_host: String::new(),
            ssh_port: 0,
            ssh_user: String::new(),
            ssh_auth: String::new(),
            ssh_key_path: String::new(),
            created_at: 0,
            updated_at: 0,
            last_used_at: None,
            has_password: false,
            has_ssh_secret: false,
            options: Default::default(),
            sentinel_password: None,
        };
        s.upsert_connection(&mut c).unwrap();
        assert!(!c.id.is_empty());
        let all = s.list_connections().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].env, "local");
        assert!(all[0].read_only);
        assert_eq!(all[0].ssl_mode, SslMode::Disable);
        s.delete_connection(&c.id).unwrap();
        assert_eq!(s.count_connections().unwrap(), 0);
    }

    #[test]
    fn history_is_pruned() {
        let s = Store::open_in_memory().unwrap();
        for i in 0..10 {
            s.add_history(
                &HistoryEntry {
                    id: 0,
                    connection_id: "c".into(),
                    database: "d".into(),
                    sql: format!("select {i}"),
                    duration_ms: 1.0,
                    rows: Some(1),
                    error: None,
                    created_at: 0,
                },
                Some(5),
            )
            .unwrap();
        }
        let h = s.list_history(Some("c"), None, 100).unwrap();
        assert_eq!(h.len(), 5);
        assert_eq!(h[0].sql, "select 9");
        let found = s.list_history(None, Some("select 7"), 100).unwrap();
        assert_eq!(found.len(), 1);
    }
}
