//! Minimal append-only log at `~/Library/Logs/Spate/host.log`.
//!
//! stdout belongs to the native messaging channel, so nothing may ever be printed there.
//! Callers must never pass header values (cookies) or full URLs to `log`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn log_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Logs/Spate/host.log"))
}

pub fn log(msg: &str) {
    if cfg!(test) {
        return;
    }
    let Some(path) = log_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{ts} [{}] {msg}", std::process::id());
    }
}

/// Host part of a URL, for logging without paths or query strings (which can carry tokens).
pub fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority)
}

#[cfg(test)]
mod tests {
    use super::url_host;

    #[test]
    fn host_only() {
        assert_eq!(
            url_host("https://user:pw@cdn.example.com:8443/a/b?token=x"),
            "cdn.example.com:8443"
        );
        assert_eq!(url_host("https://example.com?x=1"), "example.com");
        assert_eq!(url_host("nonsense"), "nonsense");
    }
}
