//! Spate daemon: download queue, SQLite history and the Unix-socket server. See CONTRACTS.md
//! ("Daemon IPC").
//!
//! The Tauri app embeds it in-process:
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! let handle = spate_daemon::Daemon::start(spate_daemon::Config::default()).await?;
//! let mut events = handle.subscribe();
//! # let req: spate_proto::AddRequest = unimplemented!();
//! let id = handle.add(req).await?;
//! while let Ok(ev) = events.recv().await {
//!     match ev {
//!         spate_daemon::DaemonEvent::Job(job) => println!("{} {:?}", job.id, job.state),
//!         spate_daemon::DaemonEvent::Removed(id) => println!("{id} removed"),
//!     }
//! }
//! handle.shutdown().await;
//! # Ok(()) }
//! ```

pub mod db;
pub mod log;
pub mod queue;
mod server;
pub mod settings;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use db::{Db, JobRow};
use spate_proto::{AddRequest, Job, JobEvent, JobState, JobView, Kind, RpcError, SegmentView, Snapshot};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

pub use settings::Settings;

/// Event names in the socket's `subscribe` pushes.
pub const EVENT_JOB: &str = "job";
pub const EVENT_REMOVED: &str = "removed";

/// What [`Handle::subscribe`] delivers. (Unboxed on purpose: nearly every event is a `Job`.)
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum DaemonEvent {
    /// Progress (≈4/s per running job) or a state change (always one event per change).
    Job(JobView),
    /// The job was dropped from the history (`remove`).
    Removed(String),
}

impl DaemonEvent {
    /// The socket push line: `{"event":"job","job":{…}}` or `{"event":"removed","job_id":"…"}`.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            DaemonEvent::Job(job) => serde_json::to_value(JobEvent { event: EVENT_JOB.into(), job: job.clone() })
                .unwrap_or(serde_json::Value::Null),
            DaemonEvent::Removed(id) => serde_json::json!({"event": EVENT_REMOVED, "job_id": id}),
        }
    }
}

/// Error codes besides those in CONTRACTS.md (`not_supported`, `unknown_method`, `bad_request`).
pub const E_NOT_FOUND: &str = "not_found";
pub const E_BAD_STATE: &str = "bad_state";
pub const E_INTERNAL: &str = "internal";

/// Put in `error` of jobs that lost their cookies when the daemon restarted.
pub const LOST_COOKIES_NOTE: &str =
    "cookies were not kept across the restart; if the server refuses, start the download again from the browser";

const TICK: Duration = Duration::from_millis(250);
/// Only downloads at least this big teach the per-host memory (small files never ramp up).
const LEARN_MIN_SIZE: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Config {
    /// Holds `spate.sqlite` and `settings.toml`.
    pub data_dir: PathBuf,
    /// Unix socket to serve; `None` = in-process only.
    pub socket_path: Option<PathBuf>,
    /// Log file; `None` = no log.
    pub log_path: Option<PathBuf>,
    /// Use these instead of reading `data_dir/settings.toml`.
    pub settings: Option<Settings>,
    /// Run `caffeinate -i` while a download is running.
    pub prevent_sleep: bool,
}

impl Default for Config {
    /// The production paths from CONTRACTS.md.
    fn default() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
        Config {
            data_dir: spate_proto::data_dir(),
            socket_path: Some(spate_proto::socket_path()),
            log_path: Some(home.join("Library/Logs/Spate/daemon.log")),
            settings: None,
            prevent_sleep: true,
        }
    }
}

impl Config {
    /// Everything under `dir` (socket `dir/spate.sock`, log `dir/daemon.log`): for tests and
    /// headless runs.
    pub fn in_dir(dir: &Path) -> Self {
        Config {
            data_dir: dir.to_path_buf(),
            socket_path: Some(dir.join("spate.sock")),
            log_path: Some(dir.join("daemon.log")),
            settings: None,
            prevent_sleep: true,
        }
    }
}

pub struct Daemon;

impl Daemon {
    /// Open the database, load the history (running/queued jobs come back paused), start the
    /// progress ticker and, if configured, the socket server. Must be called inside a tokio
    /// runtime; everything runs on it. Fails if another daemon answers on the socket.
    pub async fn start(config: Config) -> Result<Handle> {
        let logger = log::Logger::new(config.log_path.clone());
        log::route_engine_log(config.log_path.clone());
        std::fs::create_dir_all(&config.data_dir)
            .with_context(|| format!("creating {}", config.data_dir.display()))?;
        let settings = match config.settings.clone() {
            Some(s) => s.normalized(),
            None => {
                let (s, note) = Settings::load(&config.data_dir.join("settings.toml"));
                if let Some(n) = note {
                    logger.log(&n);
                }
                s
            }
        };
        // Claim the socket before touching the database: a live daemon keeps it.
        let listener = match &config.socket_path {
            Some(p) => Some(server::bind(p, &logger).await?),
            None => None,
        };
        let db = Db::open(&config.data_dir.join("spate.sqlite"))?;
        let mut jobs = Vec::new();
        for mut row in db.load_jobs()? {
            let mut changed = false;
            if matches!(row.state, JobState::Running | JobState::Queued) {
                row.state = JobState::Paused;
                changed = true;
            }
            if lost_cookies(&row) && row.state != JobState::Done && row.error.is_none() {
                row.error = Some(LOST_COOKIES_NOTE.into());
                changed = true;
            }
            if changed {
                db.save_job(&row)?;
            }
            jobs.push(Entry::new(row));
        }
        let next_seq = jobs.iter().map(|e| e.row.seq).max().unwrap_or(0) + 1;
        let (events, _) = broadcast::channel(1024);
        let n_jobs = jobs.len();
        logger.log(&format!(
            "daemon v{} started: {} jobs in history, max_concurrent={}, max_connections={}",
            spate_proto::VERSION,
            n_jobs,
            settings.max_concurrent,
            settings.max_connections
        ));
        let inner = Arc::new(Inner {
            log: logger,
            settings,
            db: Mutex::new(db),
            state: Mutex::new(State { jobs, next_seq }),
            events,
            closing: AtomicBool::new(false),
            prevent_sleep: config.prevent_sleep,
            caffeinate: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
            socket_path: config.socket_path.clone(),
        });
        let handle = Handle { inner: inner.clone() };
        let ticker = tokio::spawn(ticker(Arc::downgrade(&inner)));
        inner.tasks.lock().unwrap_or_else(|e| e.into_inner()).push(ticker);
        if let Some(l) = listener {
            let t = tokio::spawn(server::serve(l, handle.clone()));
            inner.tasks.lock().unwrap_or_else(|e| e.into_inner()).push(t);
        }
        Ok(handle)
    }
}

/// The `source` column back into the enum (stored lower-case, as in the wire format).
fn parse_source(s: &str) -> Option<spate_proto::Source> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

fn lost_cookies(row: &JobRow) -> bool {
    row.had_secrets && !row.headers.iter().any(|(k, _)| db::is_secret_header(k))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn err(code: &str, message: impl Into<String>) -> RpcError {
    RpcError { code: code.into(), message: message.into() }
}

/// What to do when a running engine returns `Paused`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum After {
    Pause,
    Cancel,
    Shutdown,
}

struct Running {
    cancel: watch::Sender<bool>,
    progress: Arc<Mutex<Snapshot>>,
    after: Option<After>,
    /// Becomes true once the job's result has been recorded.
    finished: watch::Receiver<bool>,
}

struct Entry {
    row: JobRow,
    speed_bps: f64,
    connections: usize,
    segments: Vec<SegmentView>,
    run: Option<Running>,
    /// Best speed seen while running, and the connection count at that moment.
    best: (f64, usize),
}

impl Entry {
    fn new(row: JobRow) -> Self {
        Entry { row, speed_bps: 0.0, connections: 0, segments: Vec::new(), run: None, best: (0.0, 0) }
    }

    fn view(&self) -> JobView {
        let r = &self.row;
        let filename = r
            .filename
            .clone()
            .or_else(|| spate_core::util::filename_from_url(&r.url))
            .unwrap_or_default();
        let running = r.state == JobState::Running;
        let eta_secs = match r.total {
            Some(t) if running && self.speed_bps > 1.0 && t > r.downloaded => {
                Some(((t - r.downloaded) as f64 / self.speed_bps).ceil() as u64)
            }
            _ => None,
        };
        JobView {
            id: r.id.clone(),
            url: r.url.clone(),
            filename,
            path: r.path.clone(),
            state: r.state,
            total: r.total,
            downloaded: r.downloaded,
            speed_bps: if running { self.speed_bps } else { 0.0 },
            eta_secs,
            connections: if running { self.connections } else { 0 },
            segments: self.segments.clone(),
            error: r.error.clone(),
            created_at: r.created_ms / 1000,
            finished_at: r.finished_at,
            source: parse_source(&r.source),
        }
    }

    /// Copy the engine's live snapshot in. Returns whether the file name changed.
    fn absorb(&mut self, snap: &Snapshot) -> bool {
        let mut renamed = false;
        if !snap.filename.is_empty() && self.row.filename.as_deref() != Some(snap.filename.as_str()) {
            self.row.filename = Some(snap.filename.clone());
            renamed = true;
        }
        if snap.total.is_some() {
            self.row.total = snap.total;
        }
        // Keep the last known numbers while the engine is between probe and first snapshot.
        if !snap.segments.is_empty() || snap.downloaded > 0 {
            self.row.downloaded = snap.downloaded;
            self.segments = snap.segments.clone();
        }
        self.speed_bps = snap.speed_bps;
        self.connections = snap.connections;
        if snap.speed_bps > self.best.0 && snap.connections > 0 {
            self.best = (snap.speed_bps, snap.connections);
        }
        renamed
    }
}

struct State {
    jobs: Vec<Entry>,
    next_seq: u64,
}

impl State {
    fn find(&mut self, id: &str) -> Result<&mut Entry, RpcError> {
        self.jobs.iter_mut().find(|e| e.row.id == id).ok_or_else(|| err(E_NOT_FOUND, format!("no job {id}")))
    }
}

struct Inner {
    log: log::Logger,
    settings: Settings,
    db: Mutex<Db>,
    state: Mutex<State>,
    events: broadcast::Sender<DaemonEvent>,
    closing: AtomicBool,
    prevent_sleep: bool,
    caffeinate: Mutex<Option<std::process::Child>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    socket_path: Option<PathBuf>,
}

/// Lock order: `state` before `db`.
impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, row: &JobRow) {
        if let Err(e) = self.db.lock().unwrap_or_else(|e| e.into_inner()).save_job(row) {
            self.log.log(&format!("job {}: saving to the database failed: {e:#}", row.id));
        }
    }

    fn emit(&self, e: &Entry) {
        let _ = self.events.send(DaemonEvent::Job(e.view()));
    }

    /// Persist + broadcast a state change.
    fn changed(&self, e: &Entry) {
        self.save(&e.row);
        self.emit(e);
    }

    /// Start queued jobs while slots are free.
    fn schedule(self: &Arc<Self>) {
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        let mut st = self.state();
        let list: Vec<(usize, JobState, u64)> =
            st.jobs.iter().enumerate().map(|(i, e)| (i, e.row.state, e.row.seq)).collect();
        for i in queue::to_start(&list, self.settings.max_concurrent) {
            self.start_job(&mut st.jobs[i]);
        }
        self.update_sleep_guard(&st);
    }

    fn start_job(self: &Arc<Self>, e: &mut Entry) {
        let host = log::host(&e.row.url);
        let hint = self.db.lock().unwrap_or_else(|e| e.into_inner()).host_connections(&host).ok().flatten();
        let job = Job {
            url: e.row.url.clone(),
            headers: e.row.headers.clone(),
            dir: e.row.dir.clone(),
            filename: e.row.filename.clone(),
            max_connections: self.settings.max_connections,
            start_connections: hint,
        };
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (fin_tx, fin_rx) = watch::channel(false);
        let progress = Arc::new(Mutex::new(Snapshot::default()));
        e.run = Some(Running { cancel: cancel_tx, progress: progress.clone(), after: None, finished: fin_rx });
        e.row.state = JobState::Running;
        e.row.error = lost_cookies(&e.row).then(|| LOST_COOKIES_NOTE.to_string());
        e.best = (0.0, 0);
        self.log.log(&format!("job {} start host={host} start_connections={hint:?}", e.row.id));
        self.changed(e);
        let inner = self.clone();
        let id = e.row.id.clone();
        tokio::spawn(async move {
            let result = spate_core::run(job, progress, cancel_rx).await;
            inner.finish(&id, result);
            let _ = fin_tx.send(true);
            inner.schedule();
        });
    }

    /// Record an engine result.
    fn finish(&self, id: &str, result: Result<PathBuf>) {
        let mut st = self.state();
        let Ok(e) = st.find(id) else { return };
        let Some(run) = e.run.take() else { return };
        let snap = run.progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
        e.absorb(&snap);
        e.speed_bps = 0.0;
        e.connections = 0;
        let host = log::host(&e.row.url);
        match result {
            Ok(path) => {
                e.row.state = JobState::Done;
                e.row.filename = path.file_name().map(|n| n.to_string_lossy().into_owned());
                e.row.path = Some(path);
                e.row.total = Some(e.row.total.unwrap_or(e.row.downloaded));
                e.row.downloaded = e.row.total.unwrap_or(e.row.downloaded);
                e.row.finished_at = Some(now_secs());
                e.row.error = None;
                for s in e.segments.iter_mut() {
                    s.active = false;
                }
                let (best_speed, best_conns) = e.best;
                self.log.log(&format!(
                    "job {id} done host={host} size={} best={best_speed:.0} B/s at {best_conns} connections",
                    e.row.downloaded
                ));
                if e.row.downloaded >= LEARN_MIN_SIZE && best_conns > 0 {
                    let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
                    if let Err(err) = db.set_host_connections(&host, best_conns, best_speed, now_secs()) {
                        self.log.log(&format!("saving host memory failed: {err:#}"));
                    }
                }
            }
            Err(err) => {
                let paused = err.is::<spate_core::Paused>();
                if run.after == Some(After::Cancel) {
                    remove_partials(&e.row);
                    e.row.state = JobState::Failed;
                    e.row.error = Some("cancelled".into());
                    e.segments.clear();
                    e.row.downloaded = 0;
                    self.log.log(&format!("job {id} cancelled"));
                } else if paused {
                    e.row.state = JobState::Paused;
                    self.log.log(&format!("job {id} paused ({:?})", run.after));
                } else if let Some(x) = err.downcast_ref::<spate_core::LinkExpired>() {
                    e.row.state = JobState::Failed;
                    e.row.error = Some(format!("link_expired: {x}"));
                    self.log.log(&format!("job {id} failed: link_expired (HTTP {}) host={host}", x.status));
                } else {
                    e.row.state = JobState::Failed;
                    e.row.error = Some(format!("{err:#}"));
                    self.log.log(&format!("job {id} failed host={host}: {err:#}"));
                }
                for s in e.segments.iter_mut() {
                    s.active = false;
                }
            }
        }
        self.changed(e);
        self.update_sleep_guard(&st);
    }

    /// `caffeinate -i -w <pid>` while anything runs.
    fn update_sleep_guard(&self, st: &State) {
        if !self.prevent_sleep {
            return;
        }
        let any = st.jobs.iter().any(|e| e.row.state == JobState::Running);
        let mut child = self.caffeinate.lock().unwrap_or_else(|e| e.into_inner());
        if any && child.is_none() {
            match std::process::Command::new("/usr/bin/caffeinate")
                .args(["-i", "-w", &std::process::id().to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => {
                    self.log.log(&format!("sleep prevented (caffeinate pid {})", c.id()));
                    *child = Some(c);
                }
                Err(e) => self.log.log(&format!("cannot start caffeinate: {e}")),
            }
        } else if !any {
            if let Some(mut c) = child.take() {
                let _ = c.kill();
                let _ = c.wait();
                self.log.log("sleep allowed again");
            }
        }
    }

    /// Ask a running job to stop; returns a receiver that turns true once it has.
    fn stop(&self, e: &mut Entry, why: After) -> Option<watch::Receiver<bool>> {
        let run = e.run.as_mut()?;
        // A cancel request wins over a pause.
        if run.after != Some(After::Cancel) {
            run.after = Some(why);
        }
        let _ = run.cancel.send(true);
        Some(run.finished.clone())
    }
}

/// Delete `<name>.spate-part` and its state file.
fn remove_partials(row: &JobRow) {
    let Some(name) = &row.filename else { return };
    for suffix in [spate_proto::PART_SUFFIX, spate_proto::STATE_SUFFIX] {
        let _ = std::fs::remove_file(row.dir.join(format!("{name}{suffix}")));
    }
    let _ = std::fs::remove_file(row.dir.join(format!("{name}{}.tmp", spate_proto::STATE_SUFFIX)));
}

async fn wait_finished(rx: Option<watch::Receiver<bool>>) {
    if let Some(mut rx) = rx {
        let _ = rx.wait_for(|v| *v).await;
    }
}

/// ~4 events/s per running job.
async fn ticker(inner: std::sync::Weak<Inner>) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let Some(inner) = inner.upgrade() else { return };
        let mut st = inner.state();
        for e in st.jobs.iter_mut() {
            let Some(run) = &e.run else { continue };
            let snap = run.progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if e.absorb(&snap) {
                inner.save(&e.row); // the engine settled on a file name
            }
            inner.emit(e);
        }
    }
}

/// The daemon's API, for the app in-process and for the socket server. Cheap to clone.
#[derive(Clone)]
pub struct Handle {
    inner: Arc<Inner>,
}

impl Handle {
    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    /// Every job update (≈4/s per running job and one per state change) as
    /// [`DaemonEvent::Job`] / [`DaemonEvent::Removed`].
    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.inner.events.subscribe()
    }

    /// All jobs, oldest first.
    pub fn list(&self) -> Vec<JobView> {
        let st = self.inner.state();
        let mut v: Vec<&Entry> = st.jobs.iter().collect();
        v.sort_by_key(|e| e.row.created_ms);
        v.into_iter().map(Entry::view).collect()
    }

    pub fn get(&self, id: &str) -> Option<JobView> {
        self.inner.state().jobs.iter().find(|e| e.row.id == id).map(Entry::view)
    }

    /// Queue a download; it starts right away when a slot is free. `kind: media` →
    /// `not_supported`.
    pub async fn add(&self, req: AddRequest) -> Result<String, RpcError> {
        if req.kind == Kind::Media {
            return Err(err("not_supported", "video download arrives in v2"));
        }
        let scheme_ok = req.url.starts_with("http://") || req.url.starts_with("https://");
        if !scheme_ok || req.url.len() > 16 * 1024 || log::host(&req.url) == "?" {
            return Err(err("bad_request", "url must be an http(s) URL"));
        }
        if self.inner.closing.load(Ordering::SeqCst) {
            return Err(err(E_BAD_STATE, "daemon is shutting down"));
        }
        let mut headers: Vec<(String, String)> =
            req.headers.into_iter().filter(|(k, _)| !k.trim().is_empty()).collect();
        if let Some(r) = req.referrer.filter(|r| !r.is_empty()) {
            if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("referer")) {
                headers.push(("Referer".into(), r));
            }
        }
        let had_secrets = headers.iter().any(|(k, _)| db::is_secret_header(k));
        let dir = self.inner.settings.download_dir.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let host = log::host(&req.url);
        {
            let mut st = self.inner.state();
            // Two unfinished jobs must not share a part file.
            let wanted = req
                .filename
                .as_deref()
                .filter(|n| !n.trim().is_empty())
                .map(spate_core::util::sanitize_filename)
                .or_else(|| spate_core::util::filename_from_url(&req.url).map(|n| spate_core::util::sanitize_filename(&n)));
            let explicit = req.filename.as_deref().is_some_and(|n| !n.trim().is_empty());
            let filename = match wanted {
                Some(w) => {
                    let taken = |n: &str| {
                        st.jobs.iter().any(|e| {
                            e.row.dir == dir
                                && !matches!(e.row.state, JobState::Done)
                                && e.view().filename == n
                        })
                    };
                    let unique = spate_core::util::dedup_name(&w, taken);
                    (explicit || unique != w).then_some(unique)
                }
                None => None,
            };
            let seq = st.next_seq;
            st.next_seq += 1;
            let row = JobRow {
                id: id.clone(),
                seq,
                url: req.url,
                headers,
                had_secrets,
                dir,
                filename,
                source: format!("{:?}", req.source).to_ascii_lowercase(),
                state: JobState::Queued,
                total: req.size_hint,
                downloaded: 0,
                error: None,
                created_ms: now_ms(),
                finished_at: None,
                path: None,
            };
            let e = Entry::new(row);
            self.inner.changed(&e);
            st.jobs.push(e);
        }
        self.inner.log.log(&format!("job {id} added source={:?} host={host}", req.source));
        self.inner.schedule();
        Ok(id)
    }

    /// Pause a running or queued job. Returns once the engine has stopped and saved its state.
    pub async fn pause(&self, id: &str) -> Result<(), RpcError> {
        let wait = {
            let mut st = self.inner.state();
            let e = st.find(id)?;
            match e.row.state {
                JobState::Running => self.inner.stop(e, After::Pause),
                JobState::Queued => {
                    e.row.state = JobState::Paused;
                    self.inner.changed(e);
                    None
                }
                JobState::Paused => None,
                JobState::Done | JobState::Failed => {
                    return Err(err(E_BAD_STATE, format!("job is {}", db::state_str(e.row.state))))
                }
            }
        };
        wait_finished(wait).await;
        Ok(())
    }

    /// Put a paused or failed job back in the queue (at the end); the engine resumes from its
    /// state file.
    pub async fn resume(&self, id: &str) -> Result<(), RpcError> {
        {
            let mut st = self.inner.state();
            let seq = st.next_seq;
            let e = st.find(id)?;
            match e.row.state {
                JobState::Paused | JobState::Failed => {
                    e.row.state = JobState::Queued;
                    e.row.seq = seq;
                    e.row.error = lost_cookies(&e.row).then(|| LOST_COOKIES_NOTE.to_string());
                    e.row.finished_at = None;
                    self.inner.changed(e);
                    st.next_seq += 1;
                }
                JobState::Running | JobState::Queued => return Ok(()),
                JobState::Done => return Err(err(E_BAD_STATE, "job is done")),
            }
        }
        self.inner.schedule();
        Ok(())
    }

    /// Stop the job and delete its `.spate-part` and state files. The job stays in the history
    /// as `failed` with error `cancelled`.
    pub async fn cancel(&self, id: &str) -> Result<(), RpcError> {
        let wait = {
            let mut st = self.inner.state();
            let e = st.find(id)?;
            match e.row.state {
                JobState::Running => self.inner.stop(e, After::Cancel),
                JobState::Done => return Err(err(E_BAD_STATE, "job is done")),
                JobState::Queued | JobState::Paused | JobState::Failed => {
                    remove_partials(&e.row);
                    e.row.state = JobState::Failed;
                    e.row.error = Some("cancelled".into());
                    e.row.downloaded = 0;
                    e.segments.clear();
                    self.inner.changed(e);
                    self.inner.log.log(&format!("job {id} cancelled"));
                    None
                }
            }
        };
        wait_finished(wait).await;
        Ok(())
    }

    /// Drop a done or failed job from the history. Never touches the downloaded file.
    pub async fn remove(&self, id: &str) -> Result<(), RpcError> {
        let mut st = self.inner.state();
        let idx = st
            .jobs
            .iter()
            .position(|e| e.row.id == id)
            .ok_or_else(|| err(E_NOT_FOUND, format!("no job {id}")))?;
        let state = st.jobs[idx].row.state;
        if !matches!(state, JobState::Done | JobState::Failed) {
            return Err(err(E_BAD_STATE, format!("job is {}; cancel it first", db::state_str(state))));
        }
        st.jobs.remove(idx);
        if let Err(x) = self.inner.db.lock().unwrap_or_else(|e| e.into_inner()).delete_job(id) {
            self.inner.log.log(&format!("job {id}: delete from database failed: {x:#}"));
            return Err(err(E_INTERNAL, "database error"));
        }
        let _ = self.inner.events.send(DaemonEvent::Removed(id.to_string()));
        Ok(())
    }

    /// Record that a finished job's file now lives at `path` (the app moved it, e.g. "Change
    /// folder"). Only for `done` jobs (`bad_state` otherwise); `path` must be absolute and name an
    /// existing regular file (`bad_request` otherwise). Updates `path`, `filename` and `dir` in
    /// the history and emits a `job` event. Never moves or touches the file itself.
    pub async fn set_path(&self, id: &str, path: &Path) -> Result<(), RpcError> {
        if !path.is_absolute() {
            return Err(err("bad_request", "path must be absolute"));
        }
        let mut st = self.inner.state();
        let e = st.find(id)?;
        if e.row.state != JobState::Done {
            return Err(err(E_BAD_STATE, format!("job is {}", db::state_str(e.row.state))));
        }
        match std::fs::metadata(path) {
            Ok(m) if m.is_file() => {}
            Ok(_) => return Err(err("bad_request", "path is not a regular file")),
            Err(_) => return Err(err("bad_request", "no file at path")),
        }
        e.row.path = Some(path.to_path_buf());
        if let Some(name) = path.file_name() {
            e.row.filename = Some(name.to_string_lossy().into_owned());
        }
        if let Some(parent) = path.parent() {
            e.row.dir = parent.to_path_buf();
        }
        self.inner.log.log(&format!("job {id} file moved"));
        self.inner.changed(e);
        Ok(())
    }

    /// Pause every running job (state saved), stop the socket server and the ticker. Queued
    /// and running jobs come back as `paused` on the next start.
    pub async fn shutdown(&self) {
        if self.inner.closing.swap(true, Ordering::SeqCst) {
            return;
        }
        for t in self.inner.tasks.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
            t.abort();
        }
        if let Some(p) = &self.inner.socket_path {
            let _ = std::fs::remove_file(p);
        }
        let waits: Vec<_> = {
            let mut st = self.inner.state();
            let inner = &self.inner;
            st.jobs.iter_mut().filter_map(|e| inner.stop(e, After::Shutdown)).collect()
        };
        for w in waits {
            wait_finished(Some(w)).await;
        }
        {
            let mut st = self.inner.state();
            for e in st.jobs.iter_mut() {
                if e.row.state == JobState::Queued {
                    e.row.state = JobState::Paused;
                    self.inner.save(&e.row);
                }
            }
            self.inner.update_sleep_guard(&st);
        }
        self.inner.log.log("daemon stopped");
    }
}

/// Socket method names and their dispatch, shared with tests.
pub(crate) async fn dispatch(h: &Handle, method: &str, params: serde_json::Value) -> Result<serde_json::Value, RpcError> {
    use serde_json::json;
    #[derive(serde::Deserialize)]
    struct JobId {
        job_id: String,
    }
    #[derive(serde::Deserialize)]
    struct SetPath {
        job_id: String,
        path: PathBuf,
    }
    let job_id = |p: serde_json::Value| {
        serde_json::from_value::<JobId>(p).map(|j| j.job_id).map_err(|e| err("bad_request", format!("{e}")))
    };
    match method {
        "ping" => Ok(json!({"version": spate_proto::VERSION})),
        "add" => {
            let req: AddRequest = serde_json::from_value(params).map_err(|e| err("bad_request", format!("{e}")))?;
            Ok(json!({"job_id": h.add(req).await?}))
        }
        "list" => Ok(serde_json::to_value(h.list()).map_err(|e| err(E_INTERNAL, e.to_string()))?),
        "pause" => h.pause(&job_id(params)?).await.map(|_| json!({})),
        "resume" => h.resume(&job_id(params)?).await.map(|_| json!({})),
        "cancel" => h.cancel(&job_id(params)?).await.map(|_| json!({})),
        "remove" => h.remove(&job_id(params)?).await.map(|_| json!({})),
        "set_path" => {
            let p: SetPath = serde_json::from_value(params).map_err(|e| err("bad_request", format!("{e}")))?;
            h.set_path(&p.job_id, &p.path).await.map(|_| json!({}))
        }
        "subscribe" => Ok(json!({})), // the server starts the pushes
        other => Err(err("unknown_method", other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &Path) -> Config {
        Config {
            data_dir: dir.to_path_buf(),
            socket_path: None,
            log_path: None,
            settings: Some(Settings { download_dir: dir.join("dl"), max_concurrent: 3, max_connections: 8 }),
            prevent_sleep: false,
        }
    }

    #[tokio::test]
    async fn restart_turns_running_into_paused_and_notes_lost_cookies() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = Db::open(&dir.path().join("spate.sqlite")).unwrap();
            let mut base = JobRow {
                id: "r".into(),
                seq: 1,
                url: "https://example.com/a.bin".into(),
                headers: vec![("Cookie".into(), "s=1".into())],
                had_secrets: true,
                dir: dir.path().into(),
                filename: Some("a.bin".into()),
                source: "brave".into(),
                state: JobState::Running,
                total: Some(10),
                downloaded: 5,
                error: None,
                created_ms: 1,
                finished_at: None,
                path: None,
            };
            db.save_job(&base).unwrap();
            base.id = "q".into();
            base.seq = 2;
            base.state = JobState::Queued;
            base.had_secrets = false;
            db.save_job(&base).unwrap();
            base.id = "d".into();
            base.seq = 3;
            base.state = JobState::Done;
            db.save_job(&base).unwrap();
        }
        let h = Daemon::start(cfg(dir.path())).await.unwrap();
        let list = h.list();
        let by = |id: &str| list.iter().find(|j| j.id == id).unwrap().clone();
        assert_eq!(by("r").state, JobState::Paused);
        assert_eq!(by("r").error.as_deref(), Some(LOST_COOKIES_NOTE));
        assert_eq!(by("r").downloaded, 5);
        assert_eq!(by("q").state, JobState::Paused);
        assert_eq!(by("q").error, None);
        assert_eq!(by("d").state, JobState::Done);
        assert_eq!(by("d").source, Some(spate_proto::Source::Brave), "source comes back from the database");
        // remove: only finished jobs.
        assert_eq!(h.remove("r").await.unwrap_err().code, E_BAD_STATE);
        h.remove("d").await.unwrap();
        assert!(h.get("d").is_none());
        assert_eq!(h.pause("nope").await.unwrap_err().code, E_NOT_FOUND);
        h.shutdown().await;
        let h = Daemon::start(cfg(dir.path())).await.unwrap();
        assert_eq!(h.list().len(), 2, "remove is persistent");
        h.shutdown().await;
    }

    #[tokio::test]
    async fn set_path_records_a_moved_file() {
        let dir = tempfile::tempdir().unwrap();
        let moved_dir = dir.path().join("elsewhere");
        std::fs::create_dir_all(&moved_dir).unwrap();
        let moved = moved_dir.join("a (1).bin");
        std::fs::write(&moved, b"data").unwrap();
        {
            let db = Db::open(&dir.path().join("spate.sqlite")).unwrap();
            let mut base = JobRow {
                id: "d".into(),
                seq: 1,
                url: "https://example.com/a.bin".into(),
                headers: vec![],
                had_secrets: false,
                dir: dir.path().into(),
                filename: Some("a.bin".into()),
                source: "cli".into(),
                state: JobState::Done,
                total: Some(4),
                downloaded: 4,
                error: None,
                created_ms: 1,
                finished_at: Some(2),
                path: Some(dir.path().join("a.bin")),
            };
            db.save_job(&base).unwrap();
            base.id = "p".into();
            base.seq = 2;
            base.state = JobState::Paused;
            base.path = None;
            db.save_job(&base).unwrap();
        }
        let h = Daemon::start(cfg(dir.path())).await.unwrap();
        let mut events = h.subscribe();
        let code = |r: Result<serde_json::Value, RpcError>| r.unwrap_err().code;
        let call = |params: serde_json::Value| dispatch(&h, "set_path", params);
        assert_eq!(code(call(serde_json::json!({"job_id": "d"})).await), "bad_request");
        assert_eq!(code(call(serde_json::json!({"job_id": "x", "path": moved})).await), E_NOT_FOUND);
        assert_eq!(code(call(serde_json::json!({"job_id": "p", "path": moved})).await), E_BAD_STATE);
        assert_eq!(code(call(serde_json::json!({"job_id": "d", "path": "rel/a.bin"})).await), "bad_request");
        let missing = moved_dir.join("missing.bin");
        assert_eq!(code(call(serde_json::json!({"job_id": "d", "path": missing})).await), "bad_request");
        assert_eq!(code(call(serde_json::json!({"job_id": "d", "path": moved_dir})).await), "bad_request");
        assert!(events.try_recv().is_err(), "failed calls emit nothing");

        assert_eq!(call(serde_json::json!({"job_id": "d", "path": moved})).await.unwrap(), serde_json::json!({}));
        let DaemonEvent::Job(ev) = events.try_recv().unwrap() else { panic!("expected a job event") };
        assert_eq!((ev.id.as_str(), ev.path.as_deref()), ("d", Some(moved.as_path())));
        assert_eq!(ev.filename, "a (1).bin");
        assert_eq!(ev.state, JobState::Done);
        h.shutdown().await;
        // Persisted.
        let h = Daemon::start(cfg(dir.path())).await.unwrap();
        let d = h.get("d").unwrap();
        assert_eq!(d.path.as_deref(), Some(moved.as_path()));
        assert_eq!(d.filename, "a (1).bin");
        h.shutdown().await;
    }

    #[tokio::test]
    async fn add_validation_and_queue_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path());
        c.settings.as_mut().unwrap().max_concurrent = 1;
        let h = Daemon::start(c).await.unwrap();
        let req = |url: &str, kind: Kind| AddRequest {
            url: url.into(),
            headers: vec![],
            filename: None,
            referrer: None,
            size_hint: None,
            mime: None,
            source: spate_proto::Source::Cli,
            kind,
        };
        assert_eq!(h.add(req("https://x.test/v.m3u8", Kind::Media)).await.unwrap_err().code, "not_supported");
        assert_eq!(h.add(req("ftp://x.test/a", Kind::File)).await.unwrap_err().code, "bad_request");
        // Port 9 on localhost refuses quickly; the probe retries, so the job stays running a while.
        let a = h.add(req("http://127.0.0.1:9/same.bin", Kind::File)).await.unwrap();
        let b = h.add(req("http://127.0.0.1:9/same.bin", Kind::File)).await.unwrap();
        let ja = h.get(&a).unwrap();
        let jb = h.get(&b).unwrap();
        assert_eq!(ja.state, JobState::Running);
        assert_eq!(jb.state, JobState::Queued, "max_concurrent = 1");
        assert_eq!(ja.filename, "same.bin");
        assert_eq!(ja.source, Some(spate_proto::Source::Cli));
        assert_eq!(jb.filename, "same (1).bin", "no two unfinished jobs share a part file");
        h.pause(&b).await.unwrap();
        assert_eq!(h.get(&b).unwrap().state, JobState::Paused);
        h.cancel(&a).await.unwrap();
        let ja = h.get(&a).unwrap();
        assert_eq!((ja.state, ja.error.as_deref()), (JobState::Failed, Some("cancelled")));
        // `a` freed the slot but `b` is paused: nothing runs.
        assert!(h.list().iter().all(|j| j.state != JobState::Running));
        assert_eq!(dispatch(&h, "nope", serde_json::Value::Null).await.unwrap_err().code, "unknown_method");
        assert_eq!(dispatch(&h, "pause", serde_json::json!({"id": 1})).await.unwrap_err().code, "bad_request");
        assert_eq!(dispatch(&h, "add", serde_json::json!({"url": 5})).await.unwrap_err().code, "bad_request");
        h.shutdown().await;
    }
}
