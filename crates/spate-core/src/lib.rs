//! Download engine. See CONTRACTS.md and DESIGN.md ("Technical (decided)").
//!
//! One `.spate-part` file is preallocated and every connection writes its own byte range into it
//! with `pwrite`. Segments are split dynamically when a connection is free, and the set of
//! segments (with how much of each is safely on disk) is persisted next to the part file so a
//! cancelled download resumes where it stopped.

pub mod util;
mod xattrs;

use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use spate_proto::{Job, SegmentView, Snapshot, PART_SUFFIX, STATE_SUFFIX};
use tokio::sync::watch;
use tokio::task::JoinSet;

use util::{Pick, SegInfo, MIB, MIN_PIECE};

/// Sent unless the job's headers carry their own User-Agent.
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

const BUF_SIZE: usize = 256 * 1024;
const STALL_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RETRIES: u32 = 10;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);
const TICK: Duration = Duration::from_millis(100);
const SNAPSHOT_EVERY: Duration = Duration::from_millis(250);
const SAVE_EVERY: Duration = Duration::from_secs(1);
const RAMP_EVERY: Duration = Duration::from_millis(2500);
const MAX_RESTARTS: u32 = 3;

/// Returned by [`run`] when the download was cancelled. The state is saved; calling `run`
/// again with the same job resumes.
#[derive(Debug)]
pub struct Paused;

impl std::fmt::Display for Paused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("download paused")
    }
}
impl std::error::Error for Paused {}

/// Returned when the server answers 401/403/410: the link (or its cookies) expired.
#[derive(Debug)]
pub struct LinkExpired {
    pub status: u16,
}

impl std::fmt::Display for LinkExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "link expired (HTTP {}): get a fresh link and try again", self.status)
    }
}
impl std::error::Error for LinkExpired {}

/// The server sent the whole file to a ranged request with If-Range: the file changed.
#[derive(Debug)]
struct FileChanged;

impl std::fmt::Display for FileChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the file changed on the server")
    }
}
impl std::error::Error for FileChanged {}

/// Download `job`. Progress is written into `progress`; setting `cancel` to `true` pauses
/// (returns an error that downcasts to [`Paused`]). On success returns the final path.
pub async fn run(job: Job, progress: Arc<Mutex<Snapshot>>, cancel: watch::Receiver<bool>) -> Result<PathBuf> {
    let mut restarts = 0;
    loop {
        match run_once(&job, &progress, &cancel).await {
            Err(e) if e.is::<FileChanged>() => {
                restarts += 1;
                if restarts > MAX_RESTARTS {
                    bail!("the file keeps changing on the server");
                }
            }
            other => return other,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Shared state

struct Seg {
    start: u64,
    /// Exclusive. `u64::MAX` while the size is unknown.
    end: u64,
    /// Bytes received up to here (may include bytes still in a worker's buffer).
    pos: u64,
    /// Bytes written to the part file up to here.
    flushed: u64,
    active: bool,
    /// EWMA bytes/s.
    speed: f64,
    last_pos: u64,
    retries: u32,
    spawn_pos: u64,
    not_before: Option<Instant>,
}

impl Seg {
    fn new(start: u64, end: u64, flushed: u64) -> Self {
        Seg {
            start,
            end,
            pos: flushed,
            flushed,
            active: false,
            speed: 0.0,
            last_pos: flushed,
            retries: 0,
            spawn_pos: flushed,
            not_before: None,
        }
    }
    fn done(&self) -> bool {
        self.end != u64::MAX && self.flushed >= self.end
    }
}

struct Shared {
    segs: Vec<Seg>,
    /// Monotonic count of bytes received (for throughput).
    bytes_in: u64,
}

struct Ctx {
    client: reqwest::Client,
    url: String,
    headers: HeaderMap,
    if_range: Option<HeaderValue>,
    ranged: bool,
    file: File,
    shared: Mutex<Shared>,
}

impl Ctx {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Serialize, Deserialize)]
struct PersistSeg {
    start: u64,
    end: u64,
    flushed: u64,
}

#[derive(Serialize, Deserialize)]
struct PersistState {
    url: String,
    final_url: String,
    size: u64,
    etag: Option<String>,
    last_modified: Option<String>,
    segments: Vec<PersistSeg>,
}

// ---------------------------------------------------------------------------------------------
// HTTP helpers

/// Debug log to stderr when `SPATE_LOG` is set. Never prints headers (cookies).
fn debug(args: std::fmt::Arguments<'_>) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("SPATE_LOG").is_some()) {
        eprintln!("[spate] {args}");
    }
}

/// An error with all its sources, "a: b: c".
fn chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(x) = src {
        s.push_str(": ");
        s.push_str(&x.to_string());
        src = x.source();
    }
    s
}

fn build_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .http1_only()
        .connect_timeout(CONNECT_TIMEOUT)
        .tcp_nodelay(true)
        .redirect(reqwest::redirect::Policy::limited(10))
        .pool_idle_timeout(Duration::from_secs(30))
        .build()?)
}

fn build_headers(extra: &[(String, String)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (name, value) in extra {
        let (Ok(n), Ok(v)) = (HeaderName::from_bytes(name.trim().as_bytes()), HeaderValue::from_str(value.trim()))
        else {
            continue;
        };
        if [
            header::RANGE,
            header::IF_RANGE,
            header::ACCEPT_ENCODING,
            header::HOST,
            header::CONTENT_LENGTH,
            header::CONNECTION,
        ]
        .contains(&n)
        {
            continue;
        }
        h.append(n, v);
    }
    if !h.contains_key(header::USER_AGENT) {
        h.insert(header::USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));
    }
    if !h.contains_key(header::ACCEPT) {
        h.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
    }
    h.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    h
}

/// `bytes <start>-<end>/<total|*>` or `bytes */<total>` → (start, total).
fn parse_content_range(h: &HeaderMap) -> Option<(Option<u64>, Option<u64>)> {
    let v = h.get(header::CONTENT_RANGE)?.to_str().ok()?.trim();
    let rest = v.strip_prefix("bytes")?.trim();
    let (range, total) = rest.split_once('/')?;
    let total = total.trim().parse().ok();
    let start = range.split_once('-').and_then(|(a, _)| a.trim().parse().ok());
    Some((start, total))
}

fn retry_after(h: &HeaderMap) -> Option<Duration> {
    let v = h.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let when = httpdate::parse_http_date(v).ok()?;
    Some(when.duration_since(std::time::SystemTime::now()).unwrap_or_default())
}

fn header_str(h: &HeaderMap, name: header::HeaderName) -> Option<String> {
    h.get(name).and_then(|v| v.to_str().ok()).map(|s| s.to_string())
}

struct Probe {
    final_url: String,
    size: Option<u64>,
    ranged: bool,
    etag: Option<String>,
    last_modified: Option<String>,
    disposition_name: Option<String>,
    /// For servers without range support: the full-body response, reused by the only worker.
    response: Option<reqwest::Response>,
}

async fn probe(client: &reqwest::Client, url: &str, headers: &HeaderMap) -> Result<Probe> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let last = attempt >= 4;
        let req = client.get(url).headers(headers.clone()).header(header::RANGE, "bytes=0-0");
        let resp = match tokio::time::timeout(RESPONSE_TIMEOUT, req.send()).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) if !last => {
                debug(format_args!("probe attempt {attempt} failed: {}", chain(&e)));
                tokio::time::sleep(util::backoff(attempt)).await;
                continue;
            }
            Ok(Err(e)) => return Err(e).context("connecting to the server"),
            Err(_) if !last => continue,
            Err(_) => bail!("the server did not answer"),
        };
        let status = resp.status().as_u16();
        match status {
            401 | 403 | 410 => return Err(LinkExpired { status }.into()),
            429 | 503 | 500..=599 if !last => {
                let wait = retry_after(resp.headers()).unwrap_or_else(|| util::backoff(attempt));
                tokio::time::sleep(wait.min(MAX_RETRY_AFTER)).await;
                continue;
            }
            _ => {}
        }
        let h = resp.headers();
        let mut p = Probe {
            final_url: resp.url().to_string(),
            size: None,
            ranged: false,
            etag: header_str(h, header::ETAG),
            last_modified: header_str(h, header::LAST_MODIFIED),
            disposition_name: header_str(h, header::CONTENT_DISPOSITION)
                .and_then(|v| util::filename_from_content_disposition(&v)),
            response: None,
        };
        match status {
            // An unknown total ("bytes 0-0/*") falls back to one plain stream.
            206 => {
                if let Some((_, Some(total))) = parse_content_range(h) {
                    p.size = Some(total);
                    p.ranged = true;
                }
            }
            200 => {
                p.size = resp.content_length();
                p.response = Some(resp);
            }
            416 => match parse_content_range(h) {
                Some((_, Some(0))) => p.size = Some(0),
                _ => bail!("server answered HTTP 416"),
            },
            s => bail!("server answered HTTP {s}"),
        }
        return Ok(p);
    }
}

// ---------------------------------------------------------------------------------------------
// Worker: one connection, one segment.

enum Outcome {
    Finished,
    Cancelled,
    /// `throttled`: 429/503. `refused`: no response at all (connect/TLS/request failed).
    Retry { why: String, retry_after: Option<Duration>, throttled: bool, refused: bool },
    FileChanged,
    Fatal(anyhow::Error),
}

fn retry(why: impl Into<String>) -> Outcome {
    Outcome::Retry { why: why.into(), retry_after: None, throttled: false, refused: false }
}

fn refused(why: String) -> Outcome {
    Outcome::Retry { why, retry_after: None, throttled: false, refused: true }
}

async fn wait_stop(rx: &mut watch::Receiver<bool>) {
    if rx.wait_for(|v| *v).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Write the buffer at `*off` and advance the segment's `flushed`.
fn flush(ctx: &Ctx, idx: usize, buf: &mut Vec<u8>, off: &mut u64) -> Result<()> {
    if buf.is_empty() {
        return Ok(());
    }
    ctx.file.write_all_at(buf, *off).context("writing to disk")?;
    *off += buf.len() as u64;
    buf.clear();
    ctx.lock().segs[idx].flushed = *off;
    Ok(())
}

/// Flush what we have, then report `o` (or the write error).
fn flush_then(ctx: &Ctx, idx: usize, buf: &mut Vec<u8>, off: &mut u64, o: Outcome) -> Outcome {
    match flush(ctx, idx, buf, off) {
        Ok(()) => o,
        Err(e) => Outcome::Fatal(e),
    }
}

async fn worker(
    ctx: Arc<Ctx>,
    idx: usize,
    initial: Option<reqwest::Response>,
    mut stop: watch::Receiver<bool>,
) -> Outcome {
    let (start_pos, end) = {
        let s = ctx.lock();
        (s.segs[idx].pos, s.segs[idx].end)
    };
    let resp = match initial {
        Some(r) => r,
        None => {
            let mut req = ctx.client.get(&ctx.url).headers(ctx.headers.clone());
            if ctx.ranged {
                req = req.header(header::RANGE, format!("bytes={start_pos}-{}", end - 1));
                if let Some(v) = &ctx.if_range {
                    req = req.header(header::IF_RANGE, v.clone());
                }
            }
            tokio::select! {
                r = tokio::time::timeout(RESPONSE_TIMEOUT, req.send()) => match r {
                    Err(_) => return refused("timed out waiting for the server".into()),
                    Ok(Err(e)) => return refused(format!("request failed: {}", chain(&e))),
                    Ok(Ok(r)) => r,
                },
                _ = wait_stop(&mut stop) => return Outcome::Cancelled,
            }
        }
    };
    let status = resp.status().as_u16();
    match status {
        206 if ctx.ranged => match parse_content_range(resp.headers()) {
            Some((Some(s), _)) if s == start_pos => {}
            _ => return retry("unexpected Content-Range"),
        },
        200 if ctx.ranged => return Outcome::FileChanged,
        200 if start_pos == 0 => {}
        401 | 403 | 410 => return Outcome::Fatal(LinkExpired { status }.into()),
        429 | 503 => {
            return Outcome::Retry {
                why: format!("HTTP {status}"),
                retry_after: retry_after(resp.headers()),
                throttled: true,
                refused: false,
            }
        }
        408 | 500..=599 => return retry(format!("HTTP {status}")),
        _ => return Outcome::Fatal(anyhow!("server answered HTTP {status}")),
    }

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(BUF_SIZE);
    let mut off = start_pos; // file offset of buf[0]
    let mut cur = start_pos; // received up to here
    loop {
        let next = tokio::select! {
            n = tokio::time::timeout(STALL_TIMEOUT, stream.next()) => n,
            _ = wait_stop(&mut stop) => return flush_then(&ctx, idx, &mut buf, &mut off, Outcome::Cancelled),
        };
        let chunk = match next {
            Err(_) => {
                return flush_then(&ctx, idx, &mut buf, &mut off, retry("stalled: no data for 15 s"));
            }
            Ok(Some(Err(e))) => {
                return flush_then(&ctx, idx, &mut buf, &mut off, retry(format!("connection error: {}", chain(&e))));
            }
            Ok(None) => {
                let o = flush_then(&ctx, idx, &mut buf, &mut off, Outcome::Finished);
                if !matches!(o, Outcome::Finished) {
                    return o;
                }
                let mut s = ctx.lock();
                let seg = &mut s.segs[idx];
                if seg.end == u64::MAX {
                    seg.end = cur; // size was unknown: EOF is the end
                    return Outcome::Finished;
                }
                return if cur >= seg.end { Outcome::Finished } else { retry("connection closed early") };
            }
            Ok(Some(Ok(b))) => b,
        };
        let end_now = {
            let mut s = ctx.lock();
            // The end can shrink when another connection splits this segment.
            let end_now = s.segs[idx].end;
            let take = (chunk.len() as u64).min(end_now.saturating_sub(cur)) as usize;
            buf.extend_from_slice(&chunk[..take]);
            cur += take as u64;
            s.segs[idx].pos = cur;
            s.bytes_in += take as u64;
            end_now
        };
        if buf.len() >= BUF_SIZE || cur >= end_now {
            if let Err(e) = flush(&ctx, idx, &mut buf, &mut off) {
                return Outcome::Fatal(e);
            }
        }
        if cur >= end_now {
            return Outcome::Finished;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Coordinator

fn save_state(ctx: &Ctx, path: &Path, meta: &PersistState) -> Result<()> {
    let segments: Vec<PersistSeg> = ctx
        .lock()
        .segs
        .iter()
        .map(|s| PersistSeg { start: s.start, end: s.end, flushed: s.flushed })
        .collect();
    // Everything we are about to claim as flushed must be on disk first.
    ctx.file.sync_data().context("syncing the part file")?;
    let state = PersistState {
        url: meta.url.clone(),
        final_url: meta.final_url.clone(),
        size: meta.size,
        etag: meta.etag.clone(),
        last_modified: meta.last_modified.clone(),
        segments,
    };
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let f = File::create(&tmp)?;
        serde_json::to_writer(&f, &state)?;
        f.sync_data()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

async fn save_state_async(ctx: &Arc<Ctx>, path: &Path, meta: &Arc<PersistState>) -> Result<()> {
    let (ctx, path, meta) = (ctx.clone(), path.to_path_buf(), meta.clone());
    tokio::task::spawn_blocking(move || save_state(&ctx, &path, &meta)).await?
}

fn load_state(state_path: &Path, part_path: &Path, p: &Probe) -> Option<Vec<Seg>> {
    let st: PersistState = serde_json::from_slice(&std::fs::read(state_path).ok()?).ok()?;
    if Some(st.size) != p.size || st.etag != p.etag {
        return None;
    }
    if st.etag.is_none() && st.last_modified != p.last_modified {
        return None;
    }
    if std::fs::metadata(part_path).ok()?.len() != st.size {
        return None;
    }
    let mut segs = Vec::new();
    for s in st.segments {
        if !(s.start <= s.flushed && s.flushed <= s.end && s.end <= st.size) {
            return None;
        }
        segs.push(Seg::new(s.start, s.end, s.flushed));
    }
    (!segs.is_empty()).then_some(segs)
}

fn snapshot(s: &Shared, filename: &str, total: Option<u64>, speed: f64, connections: usize) -> Snapshot {
    let mut segments: Vec<SegmentView> = s
        .segs
        .iter()
        .map(|g| SegmentView {
            start: g.start,
            end: if g.end == u64::MAX { g.pos } else { g.end },
            pos: g.pos,
            active: g.active,
        })
        .collect();
    segments.sort_by_key(|g| g.start);
    Snapshot {
        filename: filename.to_string(),
        total,
        downloaded: s.segs.iter().map(|g| g.pos.saturating_sub(g.start)).sum(),
        speed_bps: speed,
        connections,
        segments,
        done: false,
    }
}

fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.to_string()))
}

async fn stop_all(stop_tx: &watch::Sender<bool>, tasks: &mut JoinSet<(usize, Outcome)>, ctx: &Ctx) {
    let _ = stop_tx.send(true);
    while tasks.join_next().await.is_some() {}
    for s in ctx.lock().segs.iter_mut() {
        s.active = false;
        s.pos = s.flushed;
    }
}

async fn run_once(job: &Job, progress: &Arc<Mutex<Snapshot>>, cancel: &watch::Receiver<bool>) -> Result<PathBuf> {
    let set_progress = |snap: Snapshot| {
        *progress.lock().unwrap_or_else(|e| e.into_inner()) = snap;
    };
    if *cancel.borrow() {
        return Err(Paused.into());
    }
    let client = build_client()?;
    let headers = build_headers(&job.headers);
    let mut user_cancel = cancel.clone();
    let p = tokio::select! {
        p = probe(&client, &job.url, &headers) => p?,
        _ = wait_stop(&mut user_cancel) => return Err(Paused.into()),
    };

    let filename = job
        .filename
        .clone()
        .or_else(|| p.disposition_name.clone())
        .or_else(|| util::filename_from_url(&p.final_url))
        .or_else(|| util::filename_from_url(&job.url))
        .map(|n| util::sanitize_filename(&n))
        .unwrap_or_else(|| "download".to_string());
    std::fs::create_dir_all(&job.dir).with_context(|| format!("creating {}", job.dir.display()))?;
    let part_path = job.dir.join(format!("{filename}{PART_SUFFIX}"));
    let state_path = job.dir.join(format!("{filename}{STATE_SUFFIX}"));

    // Don't carry cookies/credentials to a different host we were redirected to.
    let mut seg_headers = headers.clone();
    if host_of(&p.final_url) != host_of(&job.url) {
        seg_headers.remove(header::COOKIE);
        seg_headers.remove(header::AUTHORIZATION);
    }

    let resumed = if p.ranged { load_state(&state_path, &part_path, &p) } else { None };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(resumed.is_none())
        .open(&part_path)
        .with_context(|| format!("opening {}", part_path.display()))?;
    if let Some(size) = p.size {
        file.set_len(size).context("preallocating the file")?;
    }
    if resumed.is_none() {
        let _ = std::fs::remove_file(&state_path);
    }

    let size = p.size;
    let segs = match resumed {
        Some(s) => s,
        None => match size {
            Some(size) if p.ranged && size > 0 => {
                let n = util::initial_connections(Some(size), job.max_connections) as u64;
                let piece = (size / n).div_ceil(util::ALIGN) * util::ALIGN;
                (0..n)
                    .map(|i| (i * piece, ((i + 1) * piece).min(size)))
                    .filter(|(a, b)| a < b)
                    .map(|(a, b)| Seg::new(a, b, a))
                    .collect()
            }
            Some(size) => vec![Seg::new(0, size, 0)],
            None => vec![Seg::new(0, u64::MAX, 0)],
        },
    };
    let remaining: u64 = segs.iter().map(|s| s.end.saturating_sub(s.flushed)).sum();
    let mut want = if p.ranged { util::initial_connections(Some(remaining), job.max_connections) } else { 1 };
    let mut cap = job.max_connections.max(1);

    let if_range = if p.ranged {
        p.etag
            .as_deref()
            .filter(|e| !e.starts_with("W/"))
            .or(p.last_modified.as_deref())
            .and_then(|v| HeaderValue::from_str(v).ok())
    } else {
        None
    };
    let meta = Arc::new(PersistState {
        url: job.url.clone(),
        final_url: p.final_url.clone(),
        size: size.unwrap_or(0),
        etag: p.etag.clone(),
        last_modified: p.last_modified.clone(),
        segments: vec![],
    });
    let ctx = Arc::new(Ctx {
        client,
        url: p.final_url.clone(),
        headers: seg_headers,
        if_range,
        ranged: p.ranged,
        file,
        shared: Mutex::new(Shared { segs, bytes_in: 0 }),
    });
    let mut initial_response = p.response;
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut tasks: JoinSet<(usize, Outcome)> = JoinSet::new();
    let mut cancelled = false;
    let mut cancel_closed = false;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let started = Instant::now();
    let (mut last_snap, mut last_save, mut last_ramp) = (started, started, started);
    let (mut snap_bytes, mut ramp_bytes) = (0u64, 0u64);
    let mut speed = 0.0f64;
    let mut thr_at_increase = 0.0f64;

    set_progress(snapshot(&ctx.lock(), &filename, size, 0.0, 0));

    loop {
        if !cancelled && *user_cancel.borrow() {
            cancelled = true;
            let _ = stop_tx.send(true);
        }
        // Give every free connection something to do.
        if !cancelled {
            while tasks.len() < want {
                let now = Instant::now();
                let idx = {
                    let mut s = ctx.lock();
                    // A segment waiting out its retry delay still holds its connection slot.
                    let waiting = s
                        .segs
                        .iter()
                        .filter(|g| !g.active && !g.done() && g.not_before.is_some_and(|t| t > now))
                        .count();
                    if tasks.len() + waiting >= want {
                        break;
                    }
                    let infos: Vec<SegInfo> = s
                        .segs
                        .iter()
                        .map(|g| SegInfo {
                            pos: g.pos,
                            end: g.end,
                            done: g.done(),
                            active: g.active,
                            speed: g.speed,
                            not_before: g.not_before,
                        })
                        .collect();
                    let Some(pk) = util::pick(&infos, now, MIN_PIECE, ctx.ranged) else { break };
                    let idx = match pk {
                        Pick::Resume(i) => i,
                        Pick::Split(i, at) => {
                            let old_end = s.segs[i].end;
                            s.segs[i].end = at;
                            s.segs.push(Seg::new(at, old_end, at));
                            s.segs.len() - 1
                        }
                    };
                    let g = &mut s.segs[idx];
                    g.active = true;
                    g.pos = g.flushed;
                    g.last_pos = g.pos;
                    g.spawn_pos = g.flushed;
                    g.not_before = None;
                    idx
                };
                let (c, rx, init) = (ctx.clone(), stop_rx.clone(), initial_response.take());
                debug(format_args!("start segment {idx} want={want} running={}", tasks.len() + 1));
                tasks.spawn(async move { (idx, worker(c, idx, init, rx).await) });
            }
        }

        if tasks.is_empty() {
            let all_done = ctx.lock().segs.iter().all(|s| s.done());
            if all_done {
                break;
            }
            if cancelled {
                if p.ranged {
                    save_state_async(&ctx, &state_path, &meta).await?;
                }
                set_progress(snapshot(&ctx.lock(), &filename, size, 0.0, 0));
                return Err(Paused.into());
            }
        }

        tokio::select! {
            Some(joined) = tasks.join_next(), if !tasks.is_empty() => {
                let (idx, outcome) = match joined {
                    Ok(x) => x,
                    Err(e) => {
                        stop_all(&stop_tx, &mut tasks, &ctx).await;
                        if p.ranged { let _ = save_state_async(&ctx, &state_path, &meta).await; }
                        bail!("download worker crashed: {e}");
                    }
                };
                {
                    let mut s = ctx.lock();
                    let g = &mut s.segs[idx];
                    g.active = false;
                    g.pos = g.flushed;
                    if !ctx.ranged && !matches!(outcome, Outcome::Finished) {
                        // No ranges: a broken stream starts over.
                        g.pos = 0;
                        g.flushed = 0;
                    }
                    g.last_pos = g.pos;
                }
                match outcome {
                    Outcome::Finished | Outcome::Cancelled => {}
                    Outcome::Retry { why, retry_after, throttled, refused } => {
                        debug(format_args!("segment {idx}: retry ({why})"));
                        let give_up = {
                            let mut s = ctx.lock();
                            let g = &mut s.segs[idx];
                            if g.flushed >= g.spawn_pos + MIB {
                                g.retries = 0; // it made progress; only count consecutive failures
                            }
                            g.retries += 1;
                            let delay = retry_after
                                .map(|d| d.min(MAX_RETRY_AFTER))
                                .unwrap_or_else(|| util::backoff(g.retries));
                            g.not_before = Some(Instant::now() + delay);
                            g.retries > MAX_RETRIES
                        };
                        if give_up {
                            stop_all(&stop_tx, &mut tasks, &ctx).await;
                            if p.ranged { let _ = save_state_async(&ctx, &state_path, &meta).await; }
                            bail!("giving up after {MAX_RETRIES} retries: {why}");
                        }
                        if throttled {
                            // 429/503: the server wants fewer connections; never ramp past this again.
                            cap = cap.min(want).saturating_sub(1).max(1);
                            want = want.min(cap);
                            debug(format_args!("throttled: cap={cap} want={want}"));
                        } else if refused && !tasks.is_empty() {
                            // A new connection failed while others work: likely a per-IP limit.
                            want = want.saturating_sub(1).max(tasks.len()).max(1);
                            debug(format_args!("connection refused: want={want}"));
                        }
                    }
                    Outcome::FileChanged => {
                        stop_all(&stop_tx, &mut tasks, &ctx).await;
                        drop(ctx);
                        let _ = std::fs::remove_file(&state_path);
                        let _ = std::fs::remove_file(&part_path);
                        return Err(FileChanged.into());
                    }
                    Outcome::Fatal(e) => {
                        stop_all(&stop_tx, &mut tasks, &ctx).await;
                        if p.ranged { let _ = save_state_async(&ctx, &state_path, &meta).await; }
                        return Err(e);
                    }
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                if now - last_snap >= SNAPSHOT_EVERY {
                    let dt = (now - last_snap).as_secs_f64();
                    let mut s = ctx.lock();
                    for g in s.segs.iter_mut() {
                        let inst = g.pos.saturating_sub(g.last_pos) as f64 / dt;
                        g.speed = if g.speed == 0.0 { inst } else { 0.3 * inst + 0.7 * g.speed };
                        g.last_pos = g.pos;
                    }
                    let inst = (s.bytes_in - snap_bytes) as f64 / dt;
                    speed = if speed == 0.0 { inst } else { 0.3 * inst + 0.7 * speed };
                    snap_bytes = s.bytes_in;
                    set_progress(snapshot(&s, &filename, size, speed, tasks.len()));
                    last_snap = now;
                }
                if now - last_ramp >= RAMP_EVERY {
                    let bytes_in = ctx.lock().bytes_in;
                    let thr = (bytes_in - ramp_bytes) as f64 / (now - last_ramp).as_secs_f64();
                    ramp_bytes = bytes_in;
                    last_ramp = now;
                    if !cancelled && ctx.ranged && tasks.len() >= want && want < cap
                        && thr > 0.0 && thr >= 1.1 * thr_at_increase
                    {
                        want += 1;
                        thr_at_increase = thr;
                    }
                }
                if p.ranged && now - last_save >= SAVE_EVERY {
                    save_state_async(&ctx, &state_path, &meta).await?;
                    last_save = now;
                }
            }
            r = user_cancel.changed(), if !cancelled && !cancel_closed => {
                if r.is_err() {
                    cancel_closed = true;
                }
            }
        }
    }

    // All segments are on disk.
    ctx.file.sync_all().context("syncing the file")?;
    let mut final_snap = snapshot(&ctx.lock(), &filename, size, speed, 0);
    drop(ctx);
    let final_path = util::dedup_path(&job.dir, &filename);
    std::fs::rename(&part_path, &final_path)
        .with_context(|| format!("renaming to {}", final_path.display()))?;
    let _ = std::fs::remove_file(&state_path);
    if cfg!(target_os = "macos") {
        let referrer = job
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("referer"))
            .map(|(_, v)| v.as_str());
        let name = final_path.file_name().and_then(|n| n.to_str()).unwrap_or(&filename);
        let _ = xattrs::apply(&final_path, &job.url, referrer, util::finder_tag(name));
    }
    final_snap.total = Some(final_snap.total.unwrap_or(final_snap.downloaded));
    final_snap.done = true;
    final_snap.filename = final_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(filename);
    set_progress(final_snap);
    Ok(final_path)
}
