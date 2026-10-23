//! Daemon end-to-end over its Unix socket, against a local range-capable HTTP server.
//! Needs local TCP + Unix socket binding (run outside the sandbox); skips when that is denied.

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use spate_daemon::{Config, Daemon, LOST_COOKIES_NOTE};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

// ---------------------------------------------------------------------------------------------
// Range server (a trimmed copy of the one in spate-core/tests/engine.rs)

type Body = UnsyncBoxBody<Bytes, std::io::Error>;

struct Server {
    data: Bytes,
    /// Bytes per second per connection; 0 = unlimited.
    rate: AtomicU64,
    served: AtomicU64,
    concurrent: AtomicUsize,
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

async fn handle(req: Request<Incoming>, st: Arc<Server>) -> Result<Response<Body>, Infallible> {
    let len = st.data.len() as u64;
    let range = req
        .headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok())));
    let mut b = Response::builder().header("etag", "\"v1\"").header("accept-ranges", "bytes");
    let (start, end) = match range {
        Some((a, z)) => {
            let z = z.unwrap_or(len - 1).min(len - 1);
            b = b.status(206).header("content-range", format!("bytes {a}-{z}/{len}"));
            (a, z + 1)
        }
        None => (0, len),
    };
    b = b.header("content-length", end - start);
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, std::io::Error>>(4);
    let st2 = st.clone();
    st.concurrent.fetch_add(1, SeqCst);
    tokio::spawn(async move {
        let mut pos = start;
        const CHUNK: u64 = 16 * 1024;
        while pos < end {
            let next = (pos + CHUNK).min(end);
            let rate = st2.rate.load(SeqCst);
            if rate > 0 {
                tokio::time::sleep(Duration::from_secs_f64((next - pos) as f64 / rate as f64)).await;
            }
            if tx.send(Ok(Frame::data(st2.data.slice(pos as usize..next as usize)))).await.is_err() {
                break;
            }
            st2.served.fetch_add(next - pos, SeqCst);
            pos = next;
        }
        st2.concurrent.fetch_sub(1, SeqCst);
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x, rx)) });
    Ok(b.body(StreamBody::new(stream).boxed_unsync()).unwrap())
}

/// Returns the server's base URL, or None when binding a local port is not allowed.
async fn start_server(st: Arc<Server>) -> Option<String> {
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
                let svc = hyper::service::service_fn(move |req| handle(req, st.clone()));
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(sock), svc).await;
            });
        }
    });
    Some(format!("http://{addr}"))
}

// ---------------------------------------------------------------------------------------------
// Socket client

struct Client {
    r: BufReader<tokio::net::unix::OwnedReadHalf>,
    w: tokio::net::unix::OwnedWriteHalf,
    id: u64,
}

impl Client {
    async fn connect(path: &Path) -> Client {
        let (r, w) = UnixStream::connect(path).await.expect("connect to daemon socket").into_split();
        Client { r: BufReader::new(r), w, id: 0 }
    }

    async fn line(&mut self) -> Value {
        let mut s = String::new();
        self.r.read_line(&mut s).await.unwrap();
        serde_json::from_str(s.trim()).unwrap_or_else(|e| panic!("bad line {s:?}: {e}"))
    }

    /// Full response object.
    async fn raw(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let mut req = serde_json::to_vec(&json!({"id": self.id, "method": method, "params": params})).unwrap();
        req.push(b'\n');
        self.w.write_all(&req).await.unwrap();
        let resp = self.line().await;
        assert_eq!(resp["id"], self.id, "{resp}");
        resp
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let resp = self.raw(method, params).await;
        assert!(resp.get("error").is_none(), "{method}: {resp}");
        resp["result"].clone()
    }

    async fn list(&mut self) -> Vec<Value> {
        self.call("list", Value::Null).await.as_array().unwrap().clone()
    }

    async fn job(&mut self, id: &str) -> Value {
        self.list().await.into_iter().find(|j| j["id"] == id).unwrap_or(Value::Null)
    }

    async fn wait_state(&mut self, id: &str, state: &str, max: Duration) -> Value {
        let t = Instant::now();
        loop {
            let j = self.job(id).await;
            if j["state"] == state {
                return j;
            }
            assert!(t.elapsed() < max, "job {id} never reached {state}: {j}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

static N: AtomicUsize = AtomicUsize::new(0);

/// A short directory under $TMPDIR (Unix socket paths are limited to 104 bytes).
fn tmp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("sd-{}-{}", std::process::id(), N.fetch_add(1, SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn running(list: &[Value]) -> usize {
    list.iter().filter(|j| j["state"] == "running").count()
}

fn contains_bytes(path: &Path, needle: &str) -> bool {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).contains(needle)).unwrap_or(false)
}

fn caffeinate_running() -> bool {
    let pat = format!("caffeinate -i -w {}", std::process::id());
    std::process::Command::new("/usr/bin/pgrep").args(["-f", &pat]).output().map(|o| o.status.success()).unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_pause_resume_cancel_restart() {
    let st = Arc::new(Server {
        data: Bytes::from(gen_data(12 * 1024 * 1024)),
        rate: AtomicU64::new(1024 * 1024),
        served: AtomicU64::new(0),
        concurrent: AtomicUsize::new(0),
    });
    let Some(base) = start_server(st.clone()).await else { return };
    let dir = tmp_dir();
    let dl = dir.join("dl");
    std::fs::write(
        dir.join("settings.toml"),
        format!("download_dir = {:?}\nmax_concurrent = 3\nmax_connections = 2\n", dl.to_str().unwrap()),
    )
    .unwrap();
    let cfg = Config::in_dir(&dir);
    let sock = cfg.socket_path.clone().unwrap();

    // A stale socket file (nobody listening) is replaced.
    drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
    let daemon = match Daemon::start(cfg.clone()).await {
        Ok(d) => d,
        Err(e) if format!("{e:#}").contains("Operation not permitted") => {
            eprintln!("SKIPPED: cannot bind a Unix socket here ({e:#})");
            return;
        }
        Err(e) => panic!("{e:#}"),
    };
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
    println!("socket {} mode {mode:o}", sock.display());
    assert_eq!(mode, 0o600);
    // A second daemon on the same socket refuses to start.
    let second = Daemon::start(cfg.clone()).await.err().expect("second daemon must not start");
    println!("second daemon: {second:#}");
    assert!(format!("{second:#}").contains("already running"));

    let mut c = Client::connect(&sock).await;
    assert_eq!(c.call("ping", Value::Null).await, json!({"version": "0.1.0"}));
    // Error codes.
    assert_eq!(c.raw("frobnicate", Value::Null).await["error"]["code"], "unknown_method");
    assert_eq!(c.raw("pause", json!({"jobid": 1})).await["error"]["code"], "bad_request");
    assert_eq!(c.raw("add", json!({"url": "http://x"})).await["error"]["code"], "bad_request");
    assert_eq!(
        c.raw("add", json!({"url": "https://x.test/v.m3u8", "source": "brave", "kind": "media"})).await["error"]["code"],
        "not_supported"
    );
    assert_eq!(c.raw("pause", json!({"job_id": "nope"})).await["error"]["code"], "not_found");

    // Subscriber on its own connection.
    let mut sub = Client::connect(&sock).await;
    assert_eq!(sub.call("subscribe", Value::Null).await, json!({}));
    let events: Arc<Mutex<Vec<(Instant, Value)>>> = Arc::default();
    let ev2 = events.clone();
    let sub_task = tokio::spawn(async move {
        loop {
            let v = sub.line().await;
            ev2.lock().unwrap().push((Instant::now(), v));
        }
    });

    // Four jobs; the first carries a cookie, all carry a secret query string.
    let mut ids = Vec::new();
    for i in 0..4 {
        let mut headers = vec![json!(["User-Agent", "Mozilla/5.0 test"])];
        if i == 0 {
            headers.push(json!(["Cookie", "session=COOKIE-SECRET-123"]));
        }
        let r = c
            .call(
                "add",
                json!({"url": format!("{base}/files/f{i}.bin?token=QUERYSECRET{i}"), "headers": headers,
                       "referrer": "https://example.com/page", "source": if i == 0 { "brave" } else { "cli" }, "kind": "file"}),
            )
            .await;
        ids.push(r["job_id"].as_str().unwrap().to_string());
    }

    // Watch concurrency in the background.
    let max_running = Arc::new(AtomicUsize::new(0));
    let mr = max_running.clone();
    let sock2 = sock.clone();
    let watcher = tokio::spawn(async move {
        let mut w = Client::connect(&sock2).await;
        loop {
            let n = running(&w.list().await);
            mr.fetch_max(n, SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    tokio::time::sleep(Duration::from_millis(1500)).await;
    let list = c.list().await;
    let states: Vec<&str> = list.iter().map(|j| j["state"].as_str().unwrap()).collect();
    println!("after 1.5 s: {states:?}");
    assert_eq!(states, ["running", "running", "running", "queued"]);
    assert!(caffeinate_running(), "caffeinate should run while downloading");

    // Pause job 0: the queued job 3 takes its slot.
    c.call("pause", json!({"job_id": ids[0]})).await;
    let j0 = c.job(&ids[0]).await;
    assert_eq!(j0["state"], "paused");
    let part0 = dl.join("f0.bin.spate-part");
    assert!(part0.exists() && dl.join("f0.bin.spate-part.json").exists(), "pause keeps the part + state files");
    println!("paused job 0 at {} bytes", j0["downloaded"]);
    c.wait_state(&ids[3], "running", Duration::from_secs(2)).await;

    // Cancel job 1: files gone, job stays as failed/cancelled.
    tokio::time::sleep(Duration::from_millis(500)).await;
    c.call("cancel", json!({"job_id": ids[1]})).await;
    let j1 = c.job(&ids[1]).await;
    assert_eq!((j1["state"].as_str(), j1["error"].as_str()), (Some("failed"), Some("cancelled")));
    assert!(!dl.join("f1.bin.spate-part").exists() && !dl.join("f1.bin.spate-part.json").exists());

    // Resume job 0: a slot is free, it runs again and continues from its state file.
    c.call("resume", json!({"job_id": ids[0]})).await;
    c.wait_state(&ids[0], "running", Duration::from_secs(2)).await;

    // Job 2 finishes.
    let j2 = c.wait_state(&ids[2], "done", Duration::from_secs(30)).await;
    let p2 = PathBuf::from(j2["path"].as_str().unwrap());
    assert_eq!(sha(&std::fs::read(&p2).unwrap()), sha(&st.data));
    assert_eq!(j2["downloaded"], st.data.len() as u64);

    // Restart while 0 and 3 are running.
    let before: Vec<Value> = c.list().await;
    println!(
        "before restart: {:?}",
        before.iter().map(|j| (j["state"].as_str().unwrap().to_string(), j["downloaded"].clone())).collect::<Vec<_>>()
    );
    watcher.abort();
    daemon.shutdown().await;
    assert!(!sock.exists(), "shutdown removes the socket");
    assert!(!caffeinate_running(), "caffeinate stopped when nothing runs");
    sub_task.abort();

    // Events: ~4/s per running job, plus state changes.
    {
        let ev = events.lock().unwrap();
        let for2: Vec<&(Instant, Value)> = ev.iter().filter(|(_, v)| v["job"]["id"] == ids[2]).collect();
        let first_running = for2.iter().find(|(_, v)| v["job"]["state"] == "running").unwrap().0;
        let done_at = for2.iter().find(|(_, v)| v["job"]["state"] == "done").unwrap().0;
        let running_events = for2.iter().filter(|(_, v)| v["job"]["state"] == "running").count();
        let secs = (done_at - first_running).as_secs_f64();
        println!(
            "{} events total; job 2: {running_events} running events over {secs:.1} s ({:.1}/s), states seen {:?}",
            ev.len(),
            running_events as f64 / secs,
            for2.iter().map(|(_, v)| v["job"]["state"].as_str().unwrap()).fold(Vec::new(), |mut a, s| {
                if a.last() != Some(&s) {
                    a.push(s)
                }
                a
            })
        );
        assert!(ev.iter().all(|(_, v)| v["event"] == "job"));
        let rate = running_events as f64 / secs;
        assert!((3.0..=5.5).contains(&rate), "push rate {rate}/s");
        // Every state change produced an event.
        for (id, state) in [(&ids[0], "paused"), (&ids[1], "failed"), (&ids[2], "done"), (&ids[3], "running")] {
            assert!(ev.iter().any(|(_, v)| v["job"]["id"] == **id && v["job"]["state"] == state), "{id} {state}");
        }
    }

    let daemon = Daemon::start(cfg.clone()).await.unwrap();
    let mut c = Client::connect(&sock).await;
    let after = c.list().await;
    let states: Vec<&str> = after.iter().map(|j| j["state"].as_str().unwrap()).collect();
    println!("after restart: {states:?}");
    assert_eq!(states, ["paused", "failed", "done", "paused"]);
    assert_eq!(after[0]["error"], LOST_COOKIES_NOTE, "job 0 lost its cookie");
    assert_eq!(after[3]["error"], Value::Null);
    assert_eq!(after[1]["error"], "cancelled");
    assert_eq!(after[2]["path"], j2["path"]);

    // Secrets never hit the disk: not in the database (file + WAL), not in the log.
    for f in ["spate.sqlite", "spate.sqlite-wal", "daemon.log"] {
        let p = dir.join(f);
        assert!(!contains_bytes(&p, "COOKIE-SECRET"), "cookie in {f}");
        if f == "daemon.log" {
            assert!(!contains_bytes(&p, "QUERYSECRET"), "query string in {f}");
            assert!(!contains_bytes(&p, "/files/"), "URL path in {f}");
        }
    }
    let conn = rusqlite_query(&dir.join("spate.sqlite"));
    println!("headers column after restart: {conn:?}");
    assert!(conn.iter().all(|h| !h.contains("COOKIE-SECRET")));
    assert!(conn[0].contains("Referer"), "non-secret headers are kept");

    // Resume both paused jobs and let everything finish quickly.
    st.served.store(0, SeqCst);
    st.rate.store(0, SeqCst);
    c.call("resume", json!({"job_id": ids[0]})).await;
    c.call("resume", json!({"job_id": ids[3]})).await;
    for id in [&ids[0], &ids[3]] {
        let j = c.wait_state(id, "done", Duration::from_secs(30)).await;
        let p = PathBuf::from(j["path"].as_str().unwrap());
        assert_eq!(sha(&std::fs::read(&p).unwrap()), sha(&st.data), "{id}");
        assert_eq!(j["error"], Value::Null);
    }
    let served = st.served.load(SeqCst);
    println!("resuming two jobs served {served} bytes (2 full files = {})", 2 * st.data.len());
    assert!(served < 2 * st.data.len() as u64, "resume re-downloaded everything");
    println!("max running at once: {}", max_running.load(SeqCst));
    assert_eq!(max_running.load(SeqCst), 3);

    // remove: drops the history entry, keeps the file.
    assert_eq!(c.raw("remove", json!({"job_id": ids[0]})).await["error"], Value::Null);
    assert!(c.job(&ids[0]).await.is_null());
    let p0 = after[0]["filename"].as_str().unwrap();
    assert!(dl.join(p0).exists(), "remove never deletes the file");
    let leftovers: Vec<String> = std::fs::read_dir(&dl)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    println!("download dir: {leftovers:?}");
    assert!(leftovers.iter().all(|n| !n.contains(".spate-part")));
    daemon.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

fn rusqlite_query(path: &Path) -> Vec<String> {
    // sqlite3 CLI keeps the test free of a direct rusqlite dev-dependency.
    let out = std::process::Command::new("/usr/bin/sqlite3")
        .arg(path)
        .arg("SELECT headers FROM jobs ORDER BY seq")
        .output()
        .expect("sqlite3");
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_memory_is_learned_and_used() {
    // 64 MiB (the minimum that teaches the memory) at 2 MiB/s per connection: long enough to ramp up.
    let st = Arc::new(Server {
        data: Bytes::from(gen_data(64 * 1024 * 1024)),
        rate: AtomicU64::new(2 * 1024 * 1024),
        served: AtomicU64::new(0),
        concurrent: AtomicUsize::new(0),
    });
    let Some(base) = start_server(st.clone()).await else { return };
    let dir = tmp_dir();
    let mut cfg = Config::in_dir(&dir);
    cfg.socket_path = None; // in-process only: exercises the Handle API
    cfg.prevent_sleep = false;
    cfg.settings = Some(spate_daemon::Settings { download_dir: dir.join("dl"), max_concurrent: 3, max_connections: 8 });
    let h = Daemon::start(cfg).await.unwrap();
    let mut events = h.subscribe();
    let req = |name: &str| spate_proto::AddRequest {
        url: format!("{base}/{name}"),
        headers: vec![],
        filename: None,
        referrer: None,
        size_hint: None,
        mime: None,
        source: spate_proto::Source::Paste,
        kind: spate_proto::Kind::File,
    };
    let a = h.add(req("first.bin")).await.unwrap();
    let mut best = (0.0f64, 0usize);
    let mut n_events = 0;
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(30), events.recv()).await.unwrap().unwrap();
        if ev.job.id != a {
            continue;
        }
        n_events += 1;
        if ev.job.speed_bps > best.0 {
            best = (ev.job.speed_bps, ev.job.connections);
        }
        if ev.job.state == spate_proto::JobState::Done {
            break;
        }
        assert_ne!(ev.job.state, spate_proto::JobState::Failed, "{:?}", ev.job.error);
    }
    println!("first job: {n_events} events, best {:.1} MB/s at {} connections", best.0 / 1e6, best.1);
    let learned = std::process::Command::new("/usr/bin/sqlite3")
        .arg(dir.join("spate.sqlite"))
        .arg("SELECT host, connections FROM hosts")
        .output()
        .unwrap();
    let learned = String::from_utf8_lossy(&learned.stdout).trim().to_string();
    println!("hosts table: {learned}");
    let host = base.trim_start_matches("http://");
    assert_eq!(learned, format!("{host}|{}", best.1));
    assert!(best.1 > 4, "64 MiB starts at 4 and should have ramped up");

    // The next job on this host starts with the learned count.
    let b = h.add(req("second.bin")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let jb = h.get(&b).unwrap();
    println!("second job after 1 s: {} connections", jb.connections);
    assert_eq!(jb.connections, best.1);
    let log = std::fs::read_to_string(dir.join("daemon.log")).unwrap();
    assert!(log.contains(&format!("start_connections=Some({})", best.1)), "{log}");
    h.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}
