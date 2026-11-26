//! A small log next to app.db (`rowglow.log`) for crashes and startup
//! failures, so there's something to send when Rowglow quits unexpectedly.
//! Settings → Advanced shows the folder. Kept under ~512 KB (one `.old` file).

use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 512 * 1024;

pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join("rowglow.log")
}

/// Appends one timestamped line. Never fails: logging must not cause errors.
pub fn append(data_dir: &Path, line: &str) {
    let path = log_path(data_dir);
    if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, data_dir.join("rowglow.log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(
            f,
            "{} Rowglow {} ({} {}): {}",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            line
        );
    }
}

/// Records every panic in the log, then runs the default hook (stderr).
pub fn install_panic_hook(data_dir: &Path) {
    let dir = data_dir.to_path_buf();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        let at = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        append(&dir, &format!("panic in thread '{thread}'{at}: {msg}"));
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_rotates() {
        let dir = std::env::temp_dir().join(format!("rowglow-log-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        append(&dir, "first");
        append(&dir, "second");
        let text = std::fs::read_to_string(log_path(&dir)).unwrap();
        assert!(text.contains(": first") && text.contains(": second"));
        std::fs::write(log_path(&dir), vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();
        append(&dir, "after rotation");
        assert!(dir.join("rowglow.log.old").exists());
        assert!(std::fs::read_to_string(log_path(&dir)).unwrap().contains("after rotation"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
