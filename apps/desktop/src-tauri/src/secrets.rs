//! Connection passwords. Release builds use the OS keychain (macOS Keychain,
//! Windows Credential Manager, Linux Secret Service, i.e. GNOME Keyring or
//! KWallet). If the keychain is unavailable (e.g. a Linux session without a
//! Secret Service daemon) passwords fall back to app.db. Debug builds and the dev
//! bridge keep them in app.db so rebuilt, unsigned binaries don't trigger
//! keychain access prompts on every rebuild.

use crate::error::{AppError, AppResult};
use crate::store::Store;

const SERVICE: &str = "app.rowglow.desktop";
/// Keychain services used by earlier builds; read once and copied forward.
const LEGACY_SERVICES: &[&str] = &["app.oriel.desktop", "app.querent.desktop"];

pub struct Secrets {
    use_keychain: bool,
}

impl Secrets {
    pub fn new(use_keychain: bool) -> Self {
        Secrets { use_keychain }
    }

    /// True when a keychain should be used but some secrets had to be stored
    /// in app.db instead (unencrypted, readable by this user account).
    pub fn in_file(&self, store: &Store) -> bool {
        self.use_keychain && store.secret_count().unwrap_or(0) > 0
    }

    fn entry(id: &str) -> AppResult<keyring::Entry> {
        keyring::Entry::new(SERVICE, id).map_err(|e| AppError::internal(format!("Keychain: {e}")))
    }

    pub fn get(&self, store: &Store, id: &str) -> AppResult<Option<String>> {
        if self.use_keychain {
            match Self::entry(id)?.get_password() {
                Ok(p) => return Ok(Some(p)),
                Err(keyring::Error::NoEntry) => {
                    if let Some(p) = Self::legacy_get(id) {
                        let _ = Self::entry(id).and_then(|e| {
                            e.set_password(&p).map_err(|e| AppError::internal(e.to_string()))
                        });
                        return Ok(Some(p));
                    }
                }
                Err(e) => {
                    // Fall through to the local store if the keychain is unavailable.
                    eprintln!("keychain read failed: {e}");
                }
            }
        }
        store.secret_get(id)
    }

    fn legacy_get(id: &str) -> Option<String> {
        LEGACY_SERVICES
            .iter()
            .find_map(|svc| keyring::Entry::new(svc, id).ok()?.get_password().ok())
    }

    pub fn set(&self, store: &Store, id: &str, value: &str) -> AppResult<()> {
        if self.use_keychain {
            match Self::entry(id)?.set_password(value) {
                Ok(()) => {
                    let _ = store.secret_delete(id);
                    return Ok(());
                }
                Err(e) => eprintln!("keychain write failed, using local store: {e}"),
            }
        }
        store.secret_set(id, value)
    }

    pub fn delete(&self, store: &Store, id: &str) -> AppResult<()> {
        if self.use_keychain {
            if let Ok(entry) = Self::entry(id) {
                let _ = entry.delete_credential();
            }
        }
        store.secret_delete(id)
    }
}
