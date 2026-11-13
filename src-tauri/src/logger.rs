//! Tiny append-only log file, rotated to log.old.txt past 1 MB.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static DEBUG: AtomicBool = AtomicBool::new(false);
const MAX_BYTES: u64 = 1024 * 1024;

/// Extra diagnostics (window and web view state) are on only where a `debug.on` file sits in the
/// config folder, so ordinary installs stay quiet.
pub fn set_debug(on: bool) {
    DEBUG.store(on, Ordering::SeqCst);
}

pub fn debug() -> bool {
    DEBUG.load(Ordering::SeqCst)
}

/// A line that is written only in debug mode.
pub fn trace(msg: &str) {
    if debug() {
        log(&format!("debug: {msg}"));
    }
}

pub fn init(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join("log.txt");
    if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("log.old.txt"));
    }
    *PATH.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
}

/// Where the log can't be written, the reason goes to the temp folder so it is never silent.
fn fallback(why: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("better-dictation-log-error.txt")) {
        let _ = writeln!(f, "{why}");
    }
}

pub fn log(msg: &str) {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let line = format!("[{}.{:03}] {msg}\n", ms / 1000, ms % 1000);
    if cfg!(debug_assertions) {
        eprint!("{line}");
    }
    // Opened per line so a line is on disk at once and a stuck handle can never swallow later ones.
    let path = PATH.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(path) = path {
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(line.as_bytes()) {
                    fallback(&format!("write {}: {e}", path.display()));
                }
            }
            Err(e) => fallback(&format!("open {}: {e}", path.display())),
        }
    } else {
        fallback("log before init");
    }
}
