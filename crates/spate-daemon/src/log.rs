//! Append-only daemon log (`~/Library/Logs/Spate/daemon.log` by default).
//!
//! Callers never pass header values; every message also goes through
//! [`spate_core::util::scrub_urls`], so URLs show up as scheme + host only.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// One daemon's log file (`None` = logging off).
#[derive(Clone, Default)]
pub struct Logger(Option<Arc<PathBuf>>);

impl Logger {
    pub fn new(path: Option<PathBuf>) -> Self {
        if let Some(dir) = path.as_ref().and_then(|p| p.parent()) {
            let _ = std::fs::create_dir_all(dir);
        }
        Logger(path.map(Arc::new))
    }

    pub fn log(&self, msg: &str) {
        if let Some(p) = &self.0 {
            append(p, msg);
        }
    }
}

fn append(path: &Path, msg: &str) {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{ts} [{}] {}", std::process::id(), spate_core::util::scrub_urls(msg));
    }
}

/// The engine's diagnostics are process-wide: they go to the log of the daemon started last.
static ENGINE_LOG: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn route_engine_log(path: Option<PathBuf>) {
    *ENGINE_LOG.lock().unwrap_or_else(|e| e.into_inner()) = path;
    spate_core::set_log_sink(engine);
}

fn engine(msg: &str) {
    let guard = ENGINE_LOG.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = guard.as_ref() {
        append(p, &format!("engine: {msg}"));
    }
}

/// Host (and port) of a URL, lower-cased, for log lines and the per-host memory.
pub fn host(url: &str) -> String {
    parse_host(url).unwrap_or_else(|| "?".into())
}

fn parse_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let h = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    (!h.is_empty()).then(|| h.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_only() {
        assert_eq!(super::host("https://u:p@CDN.example.com:8443/a?token=x"), "cdn.example.com:8443");
        assert_eq!(super::host("nonsense"), "?");
    }

    #[test]
    fn scrubbed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("d.log");
        super::Logger::new(Some(p.clone())).log("failed for https://h.example/x?token=SECRET");
        let text = std::fs::read_to_string(p).unwrap();
        assert!(text.contains("https://h.example/…") && !text.contains("SECRET"), "{text}");
    }
}
