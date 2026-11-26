use crate::drivers::Session;
use crate::error::{AppError, AppResult};
use crate::secrets::Secrets;
use crate::store::Store;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub struct AppState {
    pub data_dir: PathBuf,
    pub store: Store,
    pub secrets: Secrets,
    pub sessions: RwLock<HashMap<String, Arc<Session>>>,
    pub http: reqwest::Client,
    pub app_version: String,
    pub is_bridge: bool,
    /// Set when app.db was damaged at startup and moved here (the UI says so).
    pub recovered_db: Option<PathBuf>,
}

/// app.db can hold passwords (keychain fallback): keep the folder and the
/// database files readable by this user only.
#[cfg(unix)]
fn restrict_to_user(data_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700));
    for f in ["app.db", "app.db-wal", "app.db-shm"] {
        let p = data_dir.join(f);
        if p.exists() {
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
    }
}

#[cfg(not(unix))]
fn restrict_to_user(_data_dir: &std::path::Path) {}

/// Default license/update server. Set at build time with
/// `ROWGLOW_API_BASE=https://… pnpm tauri build`. Only development builds can
/// change it at runtime (Settings → Advanced).
pub fn default_api_base() -> &'static str {
    match option_env!("ROWGLOW_API_BASE") {
        // CI may export the variable empty when no repo variable/secret is set.
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => {
            // Release builds talk to no server unless one is configured: never
            // send license checks or telemetry to a domain we don't control.
            if cfg!(debug_assertions) {
                "http://localhost:3000"
            } else {
                ""
            }
        }
    }
}

/// App identifiers used by pre-launch builds (the app was called "Oriel" and,
/// before that, "Querent"), newest first. Their data is copied on first launch.
pub const LEGACY_IDENTIFIERS: &[&str] = &["app.oriel.desktop", "app.querent.desktop"];

/// First launch under the current identifier: copy app.db (saved connections,
/// history, saved queries, settings) from a previous identifier's folder, if
/// any. The old folder is left untouched as a backup.
pub fn migrate_legacy_data(data_dir: &Path) {
    if data_dir.join("app.db").exists() {
        return;
    }
    let Some(parent) = data_dir.parent() else { return };
    for id in LEGACY_IDENTIFIERS {
        let old = parent.join(id);
        if !old.join("app.db").exists() {
            continue;
        }
        if std::fs::create_dir_all(data_dir).is_err() {
            return;
        }
        for f in ["app.db", "app.db-wal", "app.db-shm"] {
            let src = old.join(f);
            if src.exists() {
                let _ = std::fs::copy(&src, data_dir.join(f));
            }
        }
        return;
    }
}

impl AppState {
    pub fn new(data_dir: PathBuf, is_bridge: bool) -> AppResult<AppState> {
        std::fs::create_dir_all(&data_dir)?;
        let (store, recovered_db) = Store::open_or_recover(&data_dir.join("app.db"))?;
        restrict_to_user(&data_dir);
        let use_keychain = !is_bridge
            && (!cfg!(debug_assertions) || std::env::var("ROWGLOW_USE_KEYCHAIN").is_ok());
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(format!("Rowglow/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| AppError::internal(e.to_string()))?;
        Ok(AppState {
            data_dir,
            store,
            secrets: Secrets::new(use_keychain),
            sessions: RwLock::new(HashMap::new()),
            http,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            is_bridge,
            recovered_db,
        })
    }

    pub fn settings(&self) -> serde_json::Map<String, serde_json::Value> {
        self.store
            .kv_get("settings")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    pub fn api_base(&self) -> String {
        // Release builds always use the server they were built for: a runtime
        // override would let anyone keep a revoked license (point it at a dead
        // server) or swap the update download link.
        if !cfg!(debug_assertions) {
            return default_api_base().to_string();
        }
        self.settings()
            .get("apiBase")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| s.starts_with("http"))
            .unwrap_or_else(|| default_api_base().to_string())
    }

    pub async fn session(&self, id: &str) -> AppResult<Arc<Session>> {
        self.sessions
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::new("connection", "This connection was closed. Reconnect to continue."))
    }

    /// Startup work: telemetry ping and license re-validation. The caller
    /// spawns it on its runtime (Tauri's async runtime in the app, Tokio in the bridge).
    pub async fn background_tasks(self: Arc<Self>) {
        crate::license::send_telemetry(&self, "launch").await;
        crate::license::maybe_refresh(&self).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_folder_once() {
        let base = std::env::temp_dir().join(format!("rowglow-migrate-{}", crate::util::new_id()));
        let old = base.join("app.querent.desktop");
        let new = base.join("app.rowglow.desktop");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("app.db"), b"legacy").unwrap();
        migrate_legacy_data(&new);
        assert_eq!(std::fs::read(new.join("app.db")).unwrap(), b"legacy");
        assert!(old.join("app.db").exists(), "old data is kept as a backup");
        // Never overwrites data that already exists under the new name.
        std::fs::write(new.join("app.db"), b"current").unwrap();
        std::fs::write(old.join("app.db"), b"legacy-2").unwrap();
        migrate_legacy_data(&new);
        assert_eq!(std::fs::read(new.join("app.db")).unwrap(), b"current");
        let _ = std::fs::remove_dir_all(&base);
    }
}
