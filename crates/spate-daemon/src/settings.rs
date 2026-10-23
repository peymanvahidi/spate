//! `settings.toml`: read once at startup; written with the defaults when missing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where downloads go. A leading `~/` is expanded.
    pub download_dir: PathBuf,
    /// Downloads running at once; the rest wait in the queue (FIFO).
    pub max_concurrent: usize,
    /// Upper bound on parallel connections per download.
    pub max_connections: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { download_dir: spate_proto::default_download_dir(), max_concurrent: 3, max_connections: 8 }
    }
}

impl Settings {
    /// Read `path`. A missing file is created with the defaults; an unreadable or invalid one
    /// falls back to the defaults (the reason is returned for the log).
    pub fn load(path: &Path) -> (Settings, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Settings>(&text) {
                Ok(s) => (s.normalized(), None),
                Err(e) => (Settings::default(), Some(format!("invalid {}: {e}; using defaults", path.display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let s = Settings::default();
                let note = toml::to_string_pretty(&s)
                    .map_err(|e| e.to_string())
                    .and_then(|t| std::fs::write(path, t).map_err(|e| e.to_string()))
                    .err()
                    .map(|e| format!("could not write default settings: {e}"));
                (s, note)
            }
            Err(e) => (Settings::default(), Some(format!("cannot read {}: {e}; using defaults", path.display()))),
        }
    }

    /// Expand `~/`, keep the counts in sane bounds.
    pub fn normalized(mut self) -> Settings {
        if let Ok(rest) = self.download_dir.strip_prefix("~") {
            if let Some(home) = std::env::var_os("HOME") {
                self.download_dir = PathBuf::from(home).join(rest);
            }
        }
        self.max_concurrent = self.max_concurrent.clamp(1, 32);
        self.max_connections = self.max_connections.clamp(1, 32);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_written_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let (s, note) = Settings::load(&path);
        assert!(note.is_none(), "{note:?}");
        assert_eq!(s.max_concurrent, 3);
        assert_eq!(s.max_connections, 8);
        assert!(s.download_dir.ends_with("Downloads/Spate"));
        let text = std::fs::read_to_string(&path).unwrap();
        println!("{text}");
        assert!(text.contains("max_concurrent = 3"));
        // Reading the written file gives the same settings.
        assert_eq!(Settings::load(&path).0, s);
    }

    #[test]
    fn partial_file_and_tilde() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "download_dir = \"~/dl\"\nmax_concurrent = 0\n").unwrap();
        let (s, note) = Settings::load(&path);
        assert!(note.is_none());
        assert_eq!(s.download_dir, PathBuf::from(std::env::var("HOME").unwrap()).join("dl"));
        assert_eq!(s.max_concurrent, 1, "clamped");
        assert_eq!(s.max_connections, 8, "missing key takes the default");
    }

    #[test]
    fn invalid_file_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "max_concurrent = \"lots\"").unwrap();
        let (s, note) = Settings::load(&path);
        assert_eq!(s, Settings::default());
        assert!(note.unwrap().contains("invalid"));
    }
}
