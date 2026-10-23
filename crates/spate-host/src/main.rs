//! `spate-host`: Chrome/Brave native messaging host for Spate.
//!
//! Reads framed JSON requests from the extension on stdin, forwards them to the daemon's
//! Unix socket and writes framed replies to stdout until stdin closes. See CONTRACTS.md.

mod framing;
mod host;
mod log;

use host::Host;
use serde_json::json;
use std::io::{self, BufWriter};
use std::process::{Command, Stdio};

fn launch_app() {
    // Don't block on `open` itself: the 1.5 s socket wait starts right away.
    // `-g` launches in the background so Brave keeps focus.
    let res = Command::new("/usr/bin/open")
        .args(["-g", "-b", spate_proto::APP_BUNDLE_ID])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match res {
        Ok(mut child) => {
            std::thread::spawn(move || {
                if !matches!(child.wait(), Ok(s) if s.success()) {
                    log::log(&format!(
                        "open -b {} failed (app not installed?)",
                        spate_proto::APP_BUNDLE_ID
                    ));
                }
            });
        }
        Err(e) => log::log(&format!("cannot run open: {e}")),
    }
}

fn main() {
    log::log(&format!("start v{}", spate_proto::VERSION));
    let mut host = Host::new(spate_proto::socket_path(), Box::new(launch_app));
    let mut stdin = io::stdin().lock();
    let mut stdout = BufWriter::new(io::stdout().lock());

    loop {
        let msg = match framing::read_message(&mut stdin) {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) => {
                log::log(&format!("read error, exiting: {e}"));
                break;
            }
        };
        let reply = host.handle(&msg);
        let mut body = serde_json::to_vec(&reply).unwrap_or_default();
        if body.len() > framing::MAX_RESPONSE {
            body = serde_json::to_vec(&json!({"ok": false, "error": "response_too_large"}))
                .unwrap_or_default();
        }
        if let Err(e) = framing::write_message(&mut stdout, &body) {
            log::log(&format!("write error, exiting: {e}"));
            break;
        }
    }
    log::log("stdin closed, exiting");
}
