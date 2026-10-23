//! End-to-end tests against a local range-capable HTTP server (hyper) with per-connection
//! throttling, dropped connections, 503s and a changing ETag.

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use sha2::{Digest, Sha256};
use spate_proto::{Job, Snapshot};

type Body = UnsyncBoxBody<Bytes, std::io::Error>;

struct Server {
    data: Bytes,
    ranges: bool,
    /// Bytes per second per connection; 0 = unlimited.
    rate: AtomicU64,
    /// Drop this many ranged (non-probe) responses halfway through.
    drop_next: AtomicU32,
    /// Answer this many ranged (non-probe) requests with 503 + Retry-After: 1.
    busy_next: AtomicU32,
    etag: Mutex<String>,
    /// Answer 429 when this many bodies are already streaming (0 = no limit).
    conn_limit: AtomicUsize,
    rejected: AtomicU32,
    disposition: Option<String>,
    served: AtomicU64,
    concurrent: AtomicUsize,
    max_concurrent: AtomicUsize,
    if_range_seen: AtomicU32,
    if_range_mismatch: AtomicU32,
    dropped: AtomicU32,
    /// The next ranged request starting at byte 0 is served at this rate (0 = off).
    slow_zero_rate: AtomicU64,
    /// Rate applied to that slow response while it streams (0 = lift the throttle).
    slow_zero_rate_live: AtomicU64,
    /// When more than this many bodies stream at once, each gets a quarter of `rate` (0 = off).
    crowd_limit: AtomicUsize,
    /// Close the connection without an answer when the User-Agent looks like a browser.
    reject_browser_ua: AtomicBool,
    user_agents: Mutex<Vec<String>>,
    /// The next ranged request starting at byte 0 stalls 64 KiB before its end.
    stall_zero_tail: AtomicBool,
    /// The next N ranged requests stall 64 KiB before their end.
    stall_tails: AtomicU32,
    /// Start offsets of the ranged (non-probe) requests, in arrival order.
    starts: Mutex<Vec<u64>>,
}

impl Server {
    fn new(len: usize, ranges: bool) -> Arc<Self> {
        Arc::new(Server {
            data: Bytes::from(gen_data(len)),
            ranges,
            rate: AtomicU64::new(0),
            drop_next: AtomicU32::new(0),
            busy_next: AtomicU32::new(0),
            etag: Mutex::new("\"v1\"".into()),
            conn_limit: AtomicUsize::new(0),
            rejected: AtomicU32::new(0),
            disposition: None,
            served: AtomicU64::new(0),
            concurrent: AtomicUsize::new(0),
            max_concurrent: AtomicUsize::new(0),
            if_range_seen: AtomicU32::new(0),
            if_range_mismatch: AtomicU32::new(0),
            dropped: AtomicU32::new(0),
            slow_zero_rate: AtomicU64::new(0),
            slow_zero_rate_live: AtomicU64::new(0),
            crowd_limit: AtomicUsize::new(0),
            reject_browser_ua: AtomicBool::new(false),
            user_agents: Mutex::new(Vec::new()),
            stall_zero_tail: AtomicBool::new(false),
            stall_tails: AtomicU32::new(0),
            starts: Mutex::new(Vec::new()),
        })
    }
}

fn gen_data(len: usize) -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn sha(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}

fn take_one(a: &AtomicU32) -> bool {
    let mut v = a.load(SeqCst);
    while v > 0 {
        match a.compare_exchange(v, v - 1, SeqCst, SeqCst) {
            Ok(_) => return true,
            Err(now) => v = now,
        }
    }
    false
}

fn parse_range(v: &str) -> Option<(u64, Option<u64>)> {
    let (a, b) = v.strip_prefix("bytes=")?.split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()))
}

async fn handle(req: Request<Incoming>, st: Arc<Server>) -> Result<Response<Body>, Infallible> {
    let len = st.data.len() as u64;
    let etag = st.etag.lock().unwrap().clone();
    let range = req.headers().get("range").and_then(|v| v.to_str().ok()).and_then(parse_range);
    let if_range = req.headers().get("if-range").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
    if let Some(ir) = &if_range {
        st.if_range_seen.fetch_add(1, SeqCst);
        if *ir != etag {
            st.if_range_mismatch.fetch_add(1, SeqCst);
        }
    }
    let is_probe = range == Some((0, Some(0)));
    if st.ranges && range.is_some() && !is_probe && take_one(&st.busy_next) {
        let body = Full::new(Bytes::from_static(b"busy")).map_err(|e| match e {}).boxed_unsync();
        return Ok(Response::builder().status(503).header("retry-after", "1").body(body).unwrap());
    }
    let limit = st.conn_limit.load(SeqCst);
    if limit > 0 && !is_probe && st.concurrent.load(SeqCst) >= limit {
        st.rejected.fetch_add(1, SeqCst);
        let body = Full::new(Bytes::from_static(b"too many")).map_err(|e| match e {}).boxed_unsync();
        return Ok(Response::builder().status(429).body(body).unwrap());
    }
    let honour = st.ranges && range.is_some() && if_range.as_ref().is_none_or(|v| *v == etag);
    let mut b = Response::builder().header("etag", etag).header("accept-ranges", "bytes");
    if let Some(d) = &st.disposition {
        b = b.header("content-disposition", d);
    }
    let (start, end) = if honour {
        let (a, z) = range.unwrap();
        let z = z.unwrap_or(len - 1).min(len - 1);
        b = b.status(206).header("content-range", format!("bytes {a}-{z}/{len}"));
        (a, z + 1)
    } else {
        b = b.status(200);
        (0, len)
    };
    b = b.header("content-length", end - start);
    let drop_at = (honour && !is_probe && take_one(&st.drop_next)).then(|| start + (end - start) / 2);
    if honour && !is_probe {
        st.starts.lock().unwrap().push(start);
    }
    let slow = honour && !is_probe && start == 0 && st.slow_zero_rate.swap(0, SeqCst) > 0;
    let stall_at = (honour
        && !is_probe
        && ((start == 0 && st.stall_zero_tail.swap(false, SeqCst)) || take_one(&st.stall_tails)))
        .then(|| end.saturating_sub(64 * 1024));

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, std::io::Error>>(4);
    let st2 = st.clone();
    let n = st.concurrent.fetch_add(1, SeqCst) + 1;
    st.max_concurrent.fetch_max(n, SeqCst);
    tokio::spawn(async move {
        let mut pos = start;
        const CHUNK: u64 = 16 * 1024;
        while pos < end {
            if stall_at.is_some_and(|s| pos >= s) {
                tokio::time::sleep(Duration::from_secs(60)).await; // never finishes in a test
            }
            if let Some(d) = drop_at {
                if pos >= d {
                    st2.dropped.fetch_add(1, SeqCst);
                    let _ = tx.send(Err(std::io::Error::other("dropped"))).await;
                    break;
                }
            }
            let next = (pos + CHUNK).min(end);
            let mut rate = st2.rate.load(SeqCst);
            if slow {
                // Re-read so a test can lift the throttle mid-stream.
                let r = st2.slow_zero_rate_live.load(SeqCst);
                if r > 0 {
                    rate = r;
                }
            }
            let crowd = st2.crowd_limit.load(SeqCst);
            if crowd > 0 && rate > 0 && st2.concurrent.load(SeqCst) > crowd {
                rate /= 4;
            }
            if rate > 0 {
                tokio::time::sleep(Duration::from_secs_f64((next - pos) as f64 / rate as f64)).await;
            }
            if tx.send(Ok(Frame::data(st2.data.slice(pos as usize..next as usize)))).await.is_err() {
                break; // client went away
            }
            st2.served.fetch_add(next - pos, SeqCst);
            pos = next;
        }
        st2.concurrent.fetch_sub(1, SeqCst);
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x, rx)) });
    Ok(b.body(StreamBody::new(stream).boxed_unsync()).unwrap())
}

/// Start the server; returns the URL of `/files/<name>`, or `None` when the sandbox forbids
/// binding a local port (EPERM), in which case the test is skipped.
async fn start(st: Arc<Server>, name: &str) -> Option<String> {
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("SKIPPED: cannot bind a local port here ({e})");
            return None;
        }
        Err(e) => panic!("bind: {e}"),
    };
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { break };
            let st = st.clone();
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |req: Request<Incoming>| {
                    let st = st.clone();
                    async move {
                        let ua = req
                            .headers()
                            .get("user-agent")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        st.user_agents.lock().unwrap().push(ua.clone());
                        if st.reject_browser_ua.load(SeqCst) && ua.starts_with("Mozilla/") {
                            // A service error makes hyper close the connection without a reply.
                            return Err(std::io::Error::other("no browsers"));
                        }
                        handle(req, st).await.map_err(|e| match e {})
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(sock), svc).await;
            });
        }
    });
    Some(format!("http://{addr}/files/{name}"))
}

fn job(url: &str, dir: &Path) -> Job {
    Job { url: url.into(), headers: vec![], dir: dir.into(), filename: None, max_connections: 8, start_connections: None }
}

async fn run(job: Job) -> anyhow::Result<PathBuf> {
    let (_tx, rx) = tokio::sync::watch::channel(false);
    spate_core::run(job, Arc::new(Mutex::new(Snapshot::default())), rx).await
}

fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".spate-part"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn segmented_download_survives_dropped_connections() {
    // 64 MiB starts with 4 connections; at 2 MiB/s each the ramp-up should add more by splitting.
    let st = Server::new(64 * 1024 * 1024, true);
    st.rate.store(2 * 1024 * 1024, SeqCst);
    st.drop_next.store(3, SeqCst);
    let Some(url) = start(st.clone(), "file.bin").await else { return };
    let dir = tempfile::tempdir().unwrap();
    let t = std::time::Instant::now();
    let path = run(job(&url, dir.path())).await.unwrap();
    let got = std::fs::read(&path).unwrap();
    println!(
        "64 MiB in {:?}, max concurrent connections {}, dropped {}",
        t.elapsed(),
        st.max_concurrent.load(SeqCst),
        st.dropped.load(SeqCst)
    );
    assert_eq!(path.file_name().unwrap(), "file.bin");
    assert_eq!(sha(&got), sha(&st.data));
    assert_eq!(st.dropped.load(SeqCst), 3);
    assert!(st.max_concurrent.load(SeqCst) >= 5, "ramp-up never added a connection");
    assert!(leftovers(dir.path()).is_empty());
    #[cfg(target_os = "macos")]
    {
        let q = xattr::get(&path, "com.apple.quarantine").unwrap().unwrap();
        assert!(String::from_utf8(q).unwrap().starts_with("0081;"));
        assert!(xattr::get(&path, "com.apple.metadata:kMDItemWhereFroms").unwrap().is_some());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_then_resume() {
    let st = Server::new(24 * 1024 * 1024, true);
    st.rate.store(2 * 1024 * 1024, SeqCst);
    let Some(url) = start(st.clone(), "resume.bin").await else { return };
    let dir = tempfile::tempdir().unwrap();

    let progress = Arc::new(Mutex::new(Snapshot::default()));
    let (tx, rx) = tokio::sync::watch::channel(false);
    let h = tokio::spawn(spate_core::run(job(&url, dir.path()), progress.clone(), rx));
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let snap = progress.lock().unwrap().clone();
    assert!(snap.downloaded > 0 && snap.connections >= 2, "{snap:?}");
    tx.send(true).unwrap();
    let err = h.await.unwrap().unwrap_err();
    assert!(err.is::<spate_core::Paused>(), "{err:#}");

    let part = dir.path().join("resume.bin.spate-part");
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("resume.bin.spate-part.json")).unwrap()).unwrap();
    assert_eq!(std::fs::metadata(&part).unwrap().len(), st.data.len() as u64);
    let flushed: u64 = state["segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["flushed"].as_u64().unwrap() - s["start"].as_u64().unwrap())
        .sum();
    assert_eq!(state["etag"], "\"v1\"");
    println!("paused with {flushed} bytes flushed, {} segments", state["segments"].as_array().unwrap().len());
    assert!(flushed > 1024 * 1024);

    st.served.store(0, SeqCst);
    st.rate.store(0, SeqCst);
    let path = run(job(&url, dir.path())).await.unwrap();
    let served = st.served.load(SeqCst);
    println!("resume served {served} bytes of {}", st.data.len());
    assert_eq!(sha(&std::fs::read(&path).unwrap()), sha(&st.data));
    assert!(served <= st.data.len() as u64 - flushed + 1, "re-downloaded flushed bytes");
    assert!(st.if_range_seen.load(SeqCst) > 0);
    assert!(leftovers(dir.path()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changed_file_restarts_from_scratch() {
    let st = Server::new(48 * 1024 * 1024, true);
    st.rate.store(2 * 1024 * 1024, SeqCst);
    let Some(url) = start(st.clone(), "changing.bin").await else { return };
    let dir = tempfile::tempdir().unwrap();
    let st2 = st.clone();
    tokio::spawn(async move {
        // The ramp-up adds a connection at ~2.5 s; its If-Range then no longer matches.
        tokio::time::sleep(Duration::from_millis(1000)).await;
        *st2.etag.lock().unwrap() = "\"v2\"".into();
    });
    let path = run(job(&url, dir.path())).await.unwrap();
    println!(
        "If-Range mismatches: {}, served {} bytes",
        st.if_range_mismatch.load(SeqCst),
        st.served.load(SeqCst)
    );
    assert!(st.if_range_mismatch.load(SeqCst) >= 1);
    assert_eq!(sha(&std::fs::read(&path).unwrap()), sha(&st.data));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_range_server_and_content_disposition() {
    let mut st = Server::new(5 * 1024 * 1024 + 123, false);
    Arc::get_mut(&mut st).unwrap().disposition =
        Some("attachment; filename=\"fallback.pdf\"; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf".into());
    let Some(url) = start(st.clone(), "ignored").await else { return };
    let dir = tempfile::tempdir().unwrap();
    let path = run(job(&url, dir.path())).await.unwrap();
    assert_eq!(path.file_name().unwrap(), "résumé.pdf");
    assert_eq!(sha(&std::fs::read(&path).unwrap()), sha(&st.data));
    // One request only: the probe's 200 body is reused.
    assert_eq!(st.served.load(SeqCst), st.data.len() as u64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn busy_server_and_dedup() {
    let st = Server::new(12 * 1024 * 1024, true);
    st.busy_next.store(2, SeqCst);
    let Some(url) = start(st.clone(), "dup.zip").await else { return };
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dup.zip"), b"existing").unwrap();
    let path = run(job(&url, dir.path())).await.unwrap();
    assert_eq!(path.file_name().unwrap(), "dup (1).zip");
    assert_eq!(sha(&std::fs::read(&path).unwrap()), sha(&st.data));
    assert_eq!(st.busy_next.load(SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_ip_connection_limit() {
    // 100 MiB wants 4 connections; the server allows 2 and answers 429 beyond that.
    let st = Server::new(100 * 1024 * 1024, true);
    st.rate.store(16 * 1024 * 1024, SeqCst);
    st.conn_limit.store(2, SeqCst);
    let Some(url) = start(st.clone(), "limited.bin").await else { return };
    let dir = tempfile::tempdir().unwrap();
    let t = std::time::Instant::now();
    let path = run(job(&url, dir.path())).await.unwrap();
    println!("100 MiB with a 2-connection limit in {:?}, {} rejected", t.elapsed(), st.rejected.load(SeqCst));
    assert_eq!(sha(&std::fs::read(&path).unwrap()), sha(&st.data));
    assert!(st.rejected.load(SeqCst) <= 4, "kept hammering the server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn browser_ua_falls_back_to_spate_ua() {
    let st = Server::new(6 * 1024 * 1024, true);
    st.reject_browser_ua.store(true, SeqCst);
    let Some(url) = start(st.clone(), "hetzner.bin").await else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut j = job(&url, dir.path());
    j.headers.push(("User-Agent".into(), spate_core::DEFAULT_USER_AGENT.into()));
    let path = run(j).await.unwrap();
    assert_eq!(sha(&std::fs::read(&path).unwrap()), sha(&st.data));
    let uas = st.user_agents.lock().unwrap().clone();
    println!("user agents seen: {uas:?}");
    assert!(uas[0].starts_with("Mozilla/"), "first try uses the browser UA");
    assert!(uas.len() >= 2);
    assert!(uas[1..].iter().all(|u| u == spate_core::SPATE_USER_AGENT), "the rest of the job uses Spate's UA");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_connections_hint_is_used() {
    // 64 MiB would start with 4 connections by the size rule; the per-host hint says 2.
    let st = Server::new(64 * 1024 * 1024, true);
    st.rate.store(1024 * 1024, SeqCst);
    let Some(url) = start(st.clone(), "hint.bin").await else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut j = job(&url, dir.path());
    j.start_connections = Some(2);
    let progress = Arc::new(Mutex::new(Snapshot::default()));
    let (tx, rx) = tokio::sync::watch::channel(false);
    let h = tokio::spawn(spate_core::run(j, progress.clone(), rx));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let snap = progress.lock().unwrap().clone();
    tx.send(true).unwrap();
    assert!(h.await.unwrap().unwrap_err().is::<spate_core::Paused>());
    println!("connections after 1.5 s: {}, segments {}", snap.connections, snap.segments.len());
    assert_eq!(snap.connections, 2);
    assert_eq!(snap.segments.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_connection_loses_its_back_half() {
    // 12 MiB, two connections, no room to ramp (max 2). The connection for [0, 6 MiB) crawls at
    // 100 KB/s; the other finishes in ~1.5 s and then sits idle because 6 MiB is too small for a
    // normal split. Stealing must hand the idle connection the slow one's back half.
    let st = Server::new(12 * 1024 * 1024, true);
    st.rate.store(4 * 1024 * 1024, SeqCst);
    st.slow_zero_rate.store(1, SeqCst);
    st.slow_zero_rate_live.store(100 * 1024, SeqCst);
    let Some(url) = start(st.clone(), "slow.bin").await else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut j = job(&url, dir.path());
    j.max_connections = 2;
    j.start_connections = Some(2);
    let t = std::time::Instant::now();
    let h = tokio::spawn(run(j));
    let half = 6 * 1024 * 1024;
    let stolen = loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let starts = st.starts.lock().unwrap().clone();
        if let Some(s) = starts.iter().find(|&&s| s > 0 && s < half) {
            break Some((*s, t.elapsed()));
        }
        if t.elapsed() > Duration::from_secs(12) {
            break None;
        }
    };
    st.slow_zero_rate_live.store(0, SeqCst); // let the rest finish quickly
    let path = h.await.unwrap().unwrap();
    println!("stolen: {stolen:?}, request starts {:?}", st.starts.lock().unwrap());
    let (at, when) = stolen.expect("the slow connection's back half was never stolen");
    assert!(when >= Duration::from_secs(4) && when < Duration::from_secs(8), "stolen after {when:?}");
    assert!(at > 3 * 1024 * 1024, "steals the back half, got {at}");
    assert_eq!(sha(&std::fs::read(&path).unwrap()), sha(&st.data));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn throughput_fall_undoes_ramp_up() {
    // Three connections get 1 MiB/s each; a fourth makes the server give everyone a quarter.
    let st = Server::new(64 * 1024 * 1024, true);
    st.rate.store(1024 * 1024, SeqCst);
    st.crowd_limit.store(3, SeqCst);
    let Some(url) = start(st.clone(), "crowd.bin").await else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut j = job(&url, dir.path());
    j.max_connections = 4;
    j.start_connections = Some(3);
    let progress = Arc::new(Mutex::new(Snapshot::default()));
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let h = tokio::spawn(spate_core::run(j, progress.clone(), rx));
    let t = std::time::Instant::now();
    let mut seen = Vec::new();
    let mut back_to_three = None;
    while t.elapsed() < Duration::from_secs(14) {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let c = progress.lock().unwrap().connections;
        seen.push(c);
        if seen.contains(&4) && c == 3 && back_to_three.is_none() {
            back_to_three = Some(t.elapsed());
        }
        if back_to_three.is_some_and(|b| t.elapsed() > b + Duration::from_secs(3)) {
            break;
        }
    }
    // After the drop it must not ramp up again.
    let after: Vec<usize> = match back_to_three {
        Some(_) => seen.iter().copied().skip_while(|&c| c != 4).skip_while(|&c| c == 4).collect(),
        None => vec![],
    };
    st.crowd_limit.store(0, SeqCst);
    st.rate.store(0, SeqCst);
    let path = h.await.unwrap().unwrap();
    println!("connections every 250 ms: {seen:?}; back to 3 after {back_to_three:?}");
    assert!(back_to_three.is_some(), "never dropped the extra connection");
    assert!(after.iter().all(|&c| c <= 3), "ramped up again after the fall: {after:?}");
    assert_eq!(sha(&std::fs::read(&path).unwrap()), sha(&st.data));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_tail_reconnects_early() {
    // [0, 6 MiB) stalls 64 KiB before its end. Too little is left to split, so instead of
    // waiting out the 15 s stall timeout the connection is restarted once it has been slow 5 s.
    let st = Server::new(12 * 1024 * 1024, true);
    st.rate.store(4 * 1024 * 1024, SeqCst);
    st.stall_zero_tail.store(true, SeqCst);
    let Some(url) = start(st.clone(), "tail.bin").await else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut j = job(&url, dir.path());
    j.max_connections = 2;
    j.start_connections = Some(2);
    let t = std::time::Instant::now();
    let path = run(j).await.unwrap();
    let took = t.elapsed();
    let starts = st.starts.lock().unwrap().clone();
    println!("finished in {took:?}; request starts {starts:?}");
    assert_eq!(sha(&std::fs::read(&path).unwrap()), sha(&st.data));
    assert!(took < Duration::from_secs(12), "waited for the stall timeout: {took:?}");
    assert!(starts.iter().any(|&s| s > 5 * 1024 * 1024 && s < 6 * 1024 * 1024), "tail was re-requested");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_stalls_reconnect_early() {
    // Both connections stall near their ends at the same time, so the median speed collapses
    // too; "no bytes for 5 s" still counts as slow and both get reconnected.
    let st = Server::new(12 * 1024 * 1024, true);
    st.rate.store(4 * 1024 * 1024, SeqCst);
    st.stall_tails.store(2, SeqCst);
    let Some(url) = start(st.clone(), "tails.bin").await else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut j = job(&url, dir.path());
    j.max_connections = 3;
    j.start_connections = Some(2);
    let t = std::time::Instant::now();
    let path = run(j).await.unwrap();
    let took = t.elapsed();
    println!("finished in {took:?}; request starts {:?}", st.starts.lock().unwrap());
    assert_eq!(sha(&std::fs::read(&path).unwrap()), sha(&st.data));
    assert!(took < Duration::from_secs(13), "waited for the stall timeout: {took:?}");
}
