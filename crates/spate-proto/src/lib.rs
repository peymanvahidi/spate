//! Shared types for every Spate component. See CONTRACTS.md.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_BUNDLE_ID: &str = "com.spate.app";
pub const NATIVE_HOST_NAME: &str = "com.spate.host";
pub const PART_SUFFIX: &str = ".spate-part";
pub const STATE_SUFFIX: &str = ".spate-part.json";

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn data_dir() -> PathBuf {
    home().join("Library/Application Support/Spate")
}

pub fn socket_path() -> PathBuf {
    data_dir().join("spate.sock")
}

pub fn default_download_dir() -> PathBuf {
    home().join("Downloads/Spate")
}

/// A download to perform. Built by the daemon from an `AddRequest`, or by the CLI directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub url: String,
    /// Extra request headers (Cookie, Referer, User-Agent, …). Never logged.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    pub dir: PathBuf,
    #[serde(default)]
    pub filename: Option<String>,
    /// Upper bound on parallel connections (balanced mode = 8).
    pub max_connections: usize,
    /// Starting connection count learned for this host (daemon's per-host memory).
    /// `None` = use the size rule.
    #[serde(default)]
    pub start_connections: Option<usize>,
}

/// Live progress, written by the engine, read by the CLI/daemon.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub filename: String,
    pub total: Option<u64>,
    pub downloaded: u64,
    pub speed_bps: f64,
    pub connections: usize,
    /// Sorted by `start`. `pos` = bytes received up to here.
    pub segments: Vec<SegmentView>,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SegmentView {
    pub start: u64,
    /// Exclusive: the segment covers `[start, end)`. For an unknown size, `end == pos`.
    pub end: u64,
    pub pos: u64,
    #[serde(default)]
    pub active: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Brave,
    Cli,
    Paste,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    File,
    Media,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddRequest {
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub referrer: Option<String>,
    #[serde(default)]
    pub size_hint: Option<u64>,
    #[serde(default)]
    pub mime: Option<String>,
    pub source: Source,
    #[serde(default)]
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Paused,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobView {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub path: Option<PathBuf>,
    pub state: JobState,
    pub total: Option<u64>,
    pub downloaded: u64,
    pub speed_bps: f64,
    pub eta_secs: Option<u64>,
    pub connections: usize,
    pub segments: Vec<SegmentView>,
    pub error: Option<String>,
    /// Unix seconds.
    pub created_at: u64,
    pub finished_at: Option<u64>,
    /// Who added the job (`brave`, `cli`, `paste`). `None` for an unknown value.
    #[serde(default)]
    pub source: Option<Source>,
}

/// Daemon IPC envelope (newline-delimited JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Pushed to `subscribe` clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEvent {
    pub event: String,
    pub job: JobView,
}
