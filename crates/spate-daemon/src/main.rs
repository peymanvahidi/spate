//! `spate-daemon`: run the daemon headless until SIGINT/SIGTERM (then pause jobs cleanly).

use std::path::PathBuf;

use spate_daemon::{Config, Daemon};

const USAGE: &str = "usage: spate-daemon [--data-dir DIR] [--socket PATH] [--log PATH] [--no-caffeinate]

  --data-dir DIR   spate.sqlite + settings.toml (default ~/Library/Application Support/Spate);
                   with --data-dir alone, the socket and log default to DIR/spate.sock, DIR/daemon.log
  --socket PATH    Unix socket to serve
  --log PATH       log file (default ~/Library/Logs/Spate/daemon.log)";

fn parse() -> Result<Config, String> {
    let mut args = std::env::args().skip(1);
    let (mut data_dir, mut socket, mut log, mut caffeinate) = (None, None, None, true);
    while let Some(a) = args.next() {
        let mut val = |name: &str| args.next().map(PathBuf::from).ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--data-dir" => data_dir = Some(val("--data-dir")?),
            "--socket" => socket = Some(val("--socket")?),
            "--log" => log = Some(val("--log")?),
            "--no-caffeinate" => caffeinate = false,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let mut cfg = match &data_dir {
        Some(d) => Config::in_dir(d),
        None => Config::default(),
    };
    if let Some(s) = socket {
        cfg.socket_path = Some(s);
    }
    if let Some(l) = log {
        cfg.log_path = Some(l);
    }
    cfg.prevent_sleep = caffeinate;
    Ok(cfg)
}

#[tokio::main]
async fn main() {
    let cfg = match parse() {
        Ok(c) => c,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("spate-daemon: {e}");
            }
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    let socket = cfg.socket_path.clone();
    let handle = match Daemon::start(cfg).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("spate-daemon: {e:#}");
            std::process::exit(1);
        }
    };
    if let Some(s) = socket {
        eprintln!("spate-daemon {} listening on {}", spate_proto::VERSION, s.display());
    }
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    eprintln!("spate-daemon: stopping (pausing running downloads)");
    handle.shutdown().await;
}
