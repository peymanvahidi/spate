//! Request handling: translate extension messages into daemon RPC calls.

use crate::log::{log, url_host};
use serde_json::{json, Value};
use spate_proto::{AddRequest, RpcRequest, RpcResponse};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Host {
    pub socket: PathBuf,
    /// Starts the app when the socket is missing (`open -b com.spate.app` in production).
    pub launch: Box<dyn Fn()>,
    /// How long to wait for the socket after launching.
    pub launch_wait: Duration,
    /// How long to wait for a daemon reply.
    pub rpc_timeout: Duration,
    next_id: u64,
}

impl Host {
    pub fn new(socket: PathBuf, launch: Box<dyn Fn()>) -> Self {
        Host {
            socket,
            launch,
            launch_wait: Duration::from_millis(1500),
            rpc_timeout: Duration::from_secs(10),
            next_id: 1,
        }
    }

    /// Handles one raw extension message and returns the JSON reply.
    pub fn handle(&mut self, raw: &[u8]) -> Value {
        match self.dispatch(raw) {
            Ok(v) => v,
            Err(e) => json!({"ok": false, "error": e}),
        }
    }

    fn dispatch(&mut self, raw: &[u8]) -> Result<Value, String> {
        let msg: Value =
            serde_json::from_slice(raw).map_err(|e| format!("bad_request: invalid JSON ({e})"))?;
        let Value::Object(mut obj) = msg else {
            return Err("bad_request: expected a JSON object".into());
        };
        let ty = obj
            .remove("type")
            .and_then(|t| t.as_str().map(str::to_owned))
            .unwrap_or_default();
        match ty.as_str() {
            "ping" => {
                let res = self
                    .call("ping", Value::Null)
                    .inspect_err(|e| log(&format!("ping failed: {}", code(e))))?;
                let version = res.get("version").cloned().unwrap_or(Value::Null);
                log("ping ok");
                Ok(json!({"ok": true, "version": version}))
            }
            "add" => {
                let req: AddRequest = serde_json::from_value(Value::Object(obj))
                    .map_err(|e| format!("bad_request: invalid add request ({e})"))?;
                let host = url_host(&req.url).to_owned();
                let kind = req.kind;
                let params = serde_json::to_value(&req).map_err(|e| format!("bad_request: {e}"))?;
                // Headers (cookies) only ever live in `params` in memory; never logged.
                match self.call("add", params) {
                    Ok(res) => {
                        let job_id = res.get("job_id").cloned().unwrap_or(Value::Null);
                        log(&format!("add ok host={host} kind={kind:?} job={job_id}"));
                        Ok(json!({"ok": true, "job_id": job_id}))
                    }
                    Err(e) => {
                        log(&format!(
                            "add failed host={host} kind={kind:?}: {}",
                            code(&e)
                        ));
                        Err(e)
                    }
                }
            }
            "" => Err("bad_request: missing type".into()),
            other => Err(format!("unknown_type: {other}")),
        }
    }

    fn connect(&self) -> Result<UnixStream, String> {
        match UnixStream::connect(&self.socket) {
            Ok(s) => Ok(s),
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
                log("daemon socket unavailable; launching app");
                (self.launch)();
                let deadline = Instant::now() + self.launch_wait;
                while Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                    if let Ok(s) = UnixStream::connect(&self.socket) {
                        return Ok(s);
                    }
                }
                Err("app_not_running".into())
            }
            Err(e) => Err(format!("io_error: cannot connect to daemon ({e})")),
        }
    }

    /// One RPC round trip. Errors are `"<code>: <message>"` strings ready for the extension.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut stream = self.connect()?;
        let io = |e: std::io::Error| {
            if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
                "timeout: daemon did not answer".to_string()
            } else {
                format!("io_error: {e}")
            }
        };
        stream
            .set_read_timeout(Some(self.rpc_timeout))
            .map_err(io)?;
        stream
            .set_write_timeout(Some(self.rpc_timeout))
            .map_err(io)?;
        let req = RpcRequest {
            id,
            method: method.into(),
            params,
        };
        let mut line = serde_json::to_vec(&req).map_err(|e| format!("bad_request: {e}"))?;
        line.push(b'\n');
        stream.write_all(&line).map_err(io)?;
        stream.flush().map_err(io)?;

        let mut reply = String::new();
        let n = BufReader::new(&stream).read_line(&mut reply).map_err(io)?;
        if n == 0 {
            return Err("io_error: daemon closed the connection".into());
        }
        let resp: RpcResponse = serde_json::from_str(reply.trim_end())
            .map_err(|e| format!("protocol_error: bad daemon reply ({e})"))?;
        if resp.id != id {
            return Err(format!(
                "protocol_error: reply id {} != request id {id}",
                resp.id
            ));
        }
        if let Some(err) = resp.error {
            return Err(format!("{}: {}", err.code, err.message));
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }
}

/// The code part of an error string, for logs (messages could echo request data).
fn code(e: &str) -> &str {
    e.split(':').next().unwrap_or(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    static N: AtomicUsize = AtomicUsize::new(0);

    /// A fresh short directory under $TMPDIR (Unix socket paths are limited to 104 bytes on macOS).
    fn tmp_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "spate-host-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Mock daemon: answers ping/add like the contract, `not_supported` for media.
    /// Records every request line it receives.
    fn spawn_daemon(path: &Path, seen: Arc<Mutex<Vec<Value>>>) {
        let listener = UnixListener::bind(path).expect("bind unix socket (sandbox may block this)");
        thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { break };
                let seen = seen.clone();
                thread::spawn(move || {
                    let mut r = BufReader::new(&conn);
                    let mut line = String::new();
                    while r.read_line(&mut line).unwrap_or(0) > 0 {
                        let req: RpcRequest = serde_json::from_str(line.trim()).unwrap();
                        seen.lock()
                            .unwrap()
                            .push(serde_json::to_value(&req).unwrap());
                        let resp = match req.method.as_str() {
                            "ping" => json!({"id": req.id, "result": {"version": "0.1.0"}}),
                            "add" if req.params["kind"] == "media" => json!({"id": req.id,
                                "error": {"code": "not_supported", "message": "video download arrives in v2"}}),
                            "add" => {
                                json!({"id": req.id, "result": {"job_id": "11111111-2222-3333-4444-555555555555"}})
                            }
                            _ => {
                                json!({"id": req.id, "error": {"code": "unknown_method", "message": req.method}})
                            }
                        };
                        let mut out = serde_json::to_vec(&resp).unwrap();
                        out.push(b'\n');
                        (&conn).write_all(&out).unwrap();
                        line.clear();
                    }
                });
            }
        });
    }

    fn no_launch() -> Box<dyn Fn()> {
        Box::new(|| panic!("launch should not be called"))
    }

    #[test]
    fn ping_and_add_through_mock_daemon() {
        let dir = tmp_dir();
        let sock = dir.join("spate.sock");
        let seen = Arc::new(Mutex::new(Vec::new()));
        spawn_daemon(&sock, seen.clone());
        let mut host = Host::new(sock, no_launch());

        assert_eq!(
            host.handle(br#"{"type":"ping"}"#),
            json!({"ok": true, "version": "0.1.0"})
        );

        let add = json!({
            "type": "add", "url": "https://cdn.example.com/big.zip",
            "headers": [["Cookie", "session=secret"], ["User-Agent", "Brave"]],
            "referrer": "https://example.com/", "size_hint": 52428800,
            "mime": "application/zip", "source": "brave", "kind": "file"
        });
        let reply = host.handle(add.to_string().as_bytes());
        assert_eq!(
            reply,
            json!({"ok": true, "job_id": "11111111-2222-3333-4444-555555555555"})
        );

        let seen = seen.lock().unwrap();
        assert_eq!(seen[0]["method"], "ping");
        assert_eq!(seen[1]["method"], "add");
        assert_eq!(
            seen[1]["params"]["headers"][0],
            json!(["Cookie", "session=secret"])
        );
        assert_eq!(seen[1]["params"]["source"], "brave");
        assert!(
            seen[1]["params"].get("type").is_none(),
            "type must not be forwarded"
        );
        assert!(seen[1]["id"].as_u64() > seen[0]["id"].as_u64());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn daemon_errors_pass_through() {
        let dir = tmp_dir();
        let sock = dir.join("spate.sock");
        spawn_daemon(&sock, Arc::new(Mutex::new(Vec::new())));
        let mut host = Host::new(sock, no_launch());
        let reply = host.handle(
            br#"{"type":"add","url":"https://x.test/v.m3u8","source":"brave","kind":"media"}"#,
        );
        assert_eq!(
            reply,
            json!({"ok": false, "error": "not_supported: video download arrives in v2"})
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_requests() {
        let mut host = Host::new(PathBuf::from("/nonexistent/spate.sock"), no_launch());
        let err = |v: Value| v["error"].as_str().unwrap().to_owned();
        assert!(err(host.handle(b"not json")).starts_with("bad_request"));
        assert!(err(host.handle(b"[1]")).starts_with("bad_request"));
        assert!(err(host.handle(br#"{"x":1}"#)).starts_with("bad_request: missing type"));
        assert_eq!(
            err(host.handle(br#"{"type":"nope"}"#)),
            "unknown_type: nope"
        );
        // Missing url/source → rejected before touching the socket.
        assert!(err(host.handle(br#"{"type":"add"}"#)).starts_with("bad_request"));
    }

    #[test]
    fn missing_socket_launches_app_then_gives_up() {
        let dir = tmp_dir();
        let calls = Rc::new(Cell::new(0));
        let c = calls.clone();
        let mut host = Host::new(dir.join("spate.sock"), Box::new(move || c.set(c.get() + 1)));
        host.launch_wait = Duration::from_millis(200);
        let t = Instant::now();
        assert_eq!(
            host.handle(br#"{"type":"ping"}"#),
            json!({"ok": false, "error": "app_not_running"})
        );
        assert_eq!(calls.get(), 1);
        assert!(t.elapsed() >= Duration::from_millis(200));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_socket_file_counts_as_not_running() {
        let dir = tmp_dir();
        let sock = dir.join("spate.sock");
        drop(UnixListener::bind(&sock).unwrap()); // leaves a socket file nobody listens on
        let calls = Rc::new(Cell::new(0));
        let c = calls.clone();
        let mut host = Host::new(sock, Box::new(move || c.set(c.get() + 1)));
        host.launch_wait = Duration::from_millis(100);
        assert_eq!(
            host.handle(br#"{"type":"ping"}"#)["error"],
            "app_not_running"
        );
        assert_eq!(calls.get(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn app_started_by_launch_is_used() {
        let dir = tmp_dir();
        let sock = dir.join("spate.sock");
        let s = sock.clone();
        // "Launching the app" brings the mock daemon up 300 ms later.
        let launch: Box<dyn Fn()> = Box::new(move || {
            let s = s.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(300));
                spawn_daemon(&s, Arc::new(Mutex::new(Vec::new())));
            });
        });
        let mut host = Host::new(sock, launch);
        assert_eq!(
            host.handle(br#"{"type":"ping"}"#),
            json!({"ok": true, "version": "0.1.0"})
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn silent_daemon_times_out() {
        let dir = tmp_dir();
        let sock = dir.join("spate.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        thread::spawn(move || {
            let held: Vec<_> = listener.incoming().take(1).collect();
            thread::sleep(Duration::from_secs(2));
            drop(held);
        });
        let mut host = Host::new(sock, no_launch());
        host.rpc_timeout = Duration::from_millis(200);
        assert_eq!(
            host.handle(br#"{"type":"ping"}"#)["error"],
            "timeout: daemon did not answer"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
