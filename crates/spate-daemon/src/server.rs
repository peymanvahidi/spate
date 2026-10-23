//! Unix socket server: newline-delimited JSON requests, responses and `subscribe` pushes.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use spate_proto::{RpcError, RpcRequest, RpcResponse};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};

use crate::{dispatch, Handle};

/// Lines longer than this are refused (the extension's add requests are a few KB).
const MAX_LINE: usize = 1024 * 1024;

/// Does a daemon answer `ping` on `path`?
pub async fn ping(path: &Path) -> bool {
    let attempt = async {
        let mut s = UnixStream::connect(path).await.ok()?;
        s.write_all(b"{\"id\":0,\"method\":\"ping\"}\n").await.ok()?;
        let mut line = String::new();
        BufReader::new(&mut s).read_line(&mut line).await.ok()?;
        let resp: RpcResponse = serde_json::from_str(line.trim()).ok()?;
        resp.result.map(|_| ())
    };
    matches!(tokio::time::timeout(Duration::from_secs(1), attempt).await, Ok(Some(())))
}

/// Bind the socket (mode 0600). A stale socket file is removed; a live daemon makes this fail.
pub async fn bind(path: &Path, log: &crate::log::Logger) -> Result<UnixListener> {
    if path.symlink_metadata().is_ok() {
        if ping(path).await {
            bail!("another Spate daemon is already running on {}", path.display());
        }
        std::fs::remove_file(path).with_context(|| format!("removing stale socket {}", path.display()))?;
        log.log("removed a stale socket file");
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let listener = UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    log.log("listening on the daemon socket");
    Ok(listener)
}

pub async fn serve(listener: UnixListener, handle: Handle) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(connection(stream, handle.clone()));
            }
            Err(e) => {
                handle.inner.log.log(&format!("accept failed: {e}"));
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

fn line_of<T: serde::Serialize>(v: &T) -> String {
    let mut s = serde_json::to_string(v).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

fn error_response(id: u64, code: &str, message: String) -> RpcResponse {
    RpcResponse { id, result: None, error: Some(RpcError { code: code.into(), message }) }
}

async fn connection(stream: UnixStream, handle: Handle) {
    let (read, mut write) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<String>(256);
    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if write.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let mut reader = BufReader::new(read);
    let mut pushes: Option<tokio::task::JoinHandle<()>> = None;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match (&mut reader).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if buf.len() > MAX_LINE {
            let _ = tx.send(line_of(&error_response(0, "bad_request", "request too large".into()))).await;
            break;
        }
        let text = String::from_utf8_lossy(&buf);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let req: RpcRequest = match serde_json::from_str(text) {
            Ok(r) => r,
            Err(e) => {
                let id = serde_json::from_str::<Value>(text).ok().and_then(|v| v["id"].as_u64()).unwrap_or(0);
                let _ = tx.send(line_of(&error_response(id, "bad_request", format!("invalid request: {e}")))).await;
                continue;
            }
        };
        let subscribe = req.method == "subscribe";
        // Subscribe before answering so no event between the reply and the first push is lost.
        let events = subscribe.then(|| handle.subscribe());
        let resp = match dispatch(&handle, &req.method, req.params).await {
            Ok(v) => RpcResponse { id: req.id, result: Some(v), error: None },
            Err(e) => RpcResponse { id: req.id, result: None, error: Some(e) },
        };
        if tx.send(line_of(&resp)).await.is_err() {
            break;
        }
        if let (Some(events), None) = (events, &pushes) {
            pushes = Some(tokio::spawn(push(handle.clone(), events, tx.clone())));
        }
    }
    if let Some(p) = pushes {
        p.abort();
    }
    drop(tx);
    let _ = writer.await;
}

/// Current state of every job, then every event, until the client goes away.
async fn push(handle: Handle, mut events: broadcast::Receiver<crate::DaemonEvent>, tx: mpsc::Sender<String>) {
    for job in handle.list() {
        let ev = crate::DaemonEvent::Job(job).to_json();
        if tx.send(line_of(&ev)).await.is_err() {
            return;
        }
    }
    loop {
        match events.recv().await {
            Ok(ev) => {
                if tx.send(line_of(&ev.to_json())).await.is_err() {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}
