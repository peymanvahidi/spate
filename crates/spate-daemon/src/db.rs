//! SQLite history (WAL): jobs and per-host connection memory.
//!
//! Cookie / Authorization header values never reach this file: [`Db::save_job`] is the only
//! writer of the `headers` column and drops them (see [`is_secret_header`]).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use spate_proto::JobState;

/// Headers whose values stay in memory only.
pub fn is_secret_header(name: &str) -> bool {
    let n = name.trim();
    n.eq_ignore_ascii_case("cookie") || n.eq_ignore_ascii_case("authorization") || n.eq_ignore_ascii_case("proxy-authorization")
}

/// The persistent part of a job.
#[derive(Debug, Clone, PartialEq)]
pub struct JobRow {
    pub id: String,
    /// Queue order (FIFO); bumped when a job is resumed.
    pub seq: u64,
    pub url: String,
    /// In memory: everything the extension sent. On disk: without secret headers.
    pub headers: Vec<(String, String)>,
    /// The request carried Cookie/Authorization (so a job loaded from disk has lost them).
    pub had_secrets: bool,
    pub dir: PathBuf,
    /// Requested name, or the name the engine settled on once it probed the server.
    pub filename: Option<String>,
    pub source: String,
    pub state: JobState,
    pub total: Option<u64>,
    pub downloaded: u64,
    pub error: Option<String>,
    /// Unix milliseconds (orders the history; `JobView.created_at` is seconds).
    pub created_ms: u64,
    pub finished_at: Option<u64>,
    pub path: Option<PathBuf>,
}

pub fn state_str(s: JobState) -> &'static str {
    match s {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Paused => "paused",
        JobState::Done => "done",
        JobState::Failed => "failed",
    }
}

fn parse_state(s: &str) -> JobState {
    match s {
        "queued" => JobState::Queued,
        "running" => JobState::Running,
        "paused" => JobState::Paused,
        "done" => JobState::Done,
        _ => JobState::Failed,
    }
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY,
                seq INTEGER NOT NULL,
                url TEXT NOT NULL,
                headers TEXT NOT NULL,
                had_secrets INTEGER NOT NULL DEFAULT 0,
                dir TEXT NOT NULL,
                filename TEXT,
                source TEXT NOT NULL,
                state TEXT NOT NULL,
                total INTEGER,
                downloaded INTEGER NOT NULL DEFAULT 0,
                error TEXT,
                created_ms INTEGER NOT NULL,
                finished_at INTEGER,
                path TEXT
            );
            CREATE TABLE IF NOT EXISTS hosts (
                host TEXT PRIMARY KEY,
                connections INTEGER NOT NULL,
                speed_bps REAL NOT NULL,
                updated_at INTEGER NOT NULL
            );",
        )?;
        Ok(Db { conn })
    }

    /// Insert or update a job. Secret headers are dropped here.
    pub fn save_job(&self, j: &JobRow) -> Result<()> {
        let kept: Vec<&(String, String)> = j.headers.iter().filter(|(k, _)| !is_secret_header(k)).collect();
        let headers = serde_json::to_string(&kept)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO jobs (id, seq, url, headers, had_secrets, dir, filename, source, state,
                total, downloaded, error, created_ms, finished_at, path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                j.id,
                j.seq as i64,
                j.url,
                headers,
                j.had_secrets,
                j.dir.to_string_lossy(),
                j.filename,
                j.source,
                state_str(j.state),
                j.total.map(|v| v as i64),
                j.downloaded as i64,
                j.error,
                j.created_ms as i64,
                j.finished_at.map(|v| v as i64),
                j.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            ],
        )?;
        Ok(())
    }

    pub fn delete_job(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM jobs WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn load_jobs(&self) -> Result<Vec<JobRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, seq, url, headers, had_secrets, dir, filename, source, state, total, downloaded,
                    error, created_ms, finished_at, path
             FROM jobs ORDER BY created_ms",
        )?;
        let rows = st.query_map([], |r| {
            let headers: String = r.get(3)?;
            Ok(JobRow {
                id: r.get(0)?,
                seq: r.get::<_, i64>(1)? as u64,
                url: r.get(2)?,
                headers: serde_json::from_str(&headers).unwrap_or_default(),
                had_secrets: r.get(4)?,
                dir: PathBuf::from(r.get::<_, String>(5)?),
                filename: r.get(6)?,
                source: r.get(7)?,
                state: parse_state(&r.get::<_, String>(8)?),
                total: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                downloaded: r.get::<_, i64>(10)? as u64,
                error: r.get(11)?,
                created_ms: r.get::<_, i64>(12)? as u64,
                finished_at: r.get::<_, Option<i64>>(13)?.map(|v| v as u64),
                path: r.get::<_, Option<String>>(14)?.map(PathBuf::from),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Learned starting connection count for `host`.
    pub fn host_connections(&self, host: &str) -> Result<Option<usize>> {
        Ok(self
            .conn
            .query_row("SELECT connections FROM hosts WHERE host = ?1", params![host], |r| r.get::<_, i64>(0))
            .optional()?
            .map(|v| v.max(1) as usize))
    }

    pub fn set_host_connections(&self, host: &str, connections: usize, speed_bps: f64, now: u64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO hosts (host, connections, speed_bps, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![host, connections as i64, speed_bps, now as i64],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn row(id: &str) -> JobRow {
        JobRow {
            id: id.into(),
            seq: 1,
            url: "https://cdn.example.com/f.zip?sig=abc".into(),
            headers: vec![
                ("Cookie".into(), "session=TOPSECRET-COOKIE".into()),
                ("authorization".into(), "Bearer TOPSECRET-TOKEN".into()),
                ("Referer".into(), "https://example.com/".into()),
                ("User-Agent".into(), "Mozilla/5.0 test".into()),
            ],
            had_secrets: true,
            dir: "/tmp/x".into(),
            filename: Some("f.zip".into()),
            source: "brave".into(),
            state: JobState::Running,
            total: Some(100),
            downloaded: 10,
            error: None,
            created_ms: 1,
            finished_at: None,
            path: None,
        }
    }

    #[test]
    fn secret_headers() {
        assert!(is_secret_header("Cookie"));
        assert!(is_secret_header(" AUTHORIZATION "));
        assert!(is_secret_header("Proxy-Authorization"));
        assert!(!is_secret_header("Referer"));
        assert!(!is_secret_header("Set-Cookie2"));
    }

    #[test]
    fn cookie_never_reaches_the_database_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spate.sqlite");
        let db = Db::open(&path).unwrap();
        let mut r = row("a");
        db.save_job(&r).unwrap();
        r.state = JobState::Paused;
        db.save_job(&r).unwrap(); // update path too

        // Round trip: only the non-secret headers come back.
        let loaded = db.load_jobs().unwrap();
        assert_eq!(loaded.len(), 1);
        let names: Vec<&str> = loaded[0].headers.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["Referer", "User-Agent"]);
        assert!(loaded[0].had_secrets);
        assert_eq!(loaded[0].state, JobState::Paused);

        // Query the file with an independent connection.
        let other = Connection::open(&path).unwrap();
        let headers: String = other.query_row("SELECT headers FROM jobs WHERE id = 'a'", [], |r| r.get(0)).unwrap();
        println!("headers column: {headers}");
        assert!(!headers.contains("TOPSECRET"));
        let n: i64 = other
            .query_row(
                "SELECT count(*) FROM jobs WHERE headers LIKE '%TOPSECRET%' OR url LIKE '%TOPSECRET%'
                 OR coalesce(error, '') LIKE '%TOPSECRET%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        drop(other);
        // And not anywhere in the raw bytes (main file + WAL).
        for f in std::fs::read_dir(dir.path()).unwrap() {
            let f = f.unwrap().path();
            let bytes = std::fs::read(&f).unwrap();
            let hay = String::from_utf8_lossy(&bytes);
            assert!(!hay.contains("TOPSECRET"), "secret found in {}", f.display());
            println!("{}: {} bytes, clean", f.file_name().unwrap().to_string_lossy(), bytes.len());
        }
    }

    #[test]
    fn host_memory_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("spate.sqlite")).unwrap();
        assert_eq!(db.host_connections("cdn.example.com").unwrap(), None);
        db.set_host_connections("cdn.example.com", 6, 5e7, 10).unwrap();
        assert_eq!(db.host_connections("cdn.example.com").unwrap(), Some(6));
        db.set_host_connections("cdn.example.com", 3, 1e7, 11).unwrap();
        assert_eq!(db.host_connections("cdn.example.com").unwrap(), Some(3));
        assert_eq!(db.host_connections("other.example.com").unwrap(), None);
    }
}
