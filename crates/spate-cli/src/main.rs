//! `spate` command-line client: `spate get <URL>`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use spate_proto::{Job, Snapshot};

#[derive(Parser)]
#[command(name = "spate", version, about = "Spate download accelerator")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download a URL (Ctrl-C pauses; run the same command again to resume).
    Get {
        url: String,
        /// Download directory (default ~/Downloads/Spate).
        #[arg(short = 'd', long)]
        dir: Option<PathBuf>,
        /// Maximum parallel connections.
        #[arg(short = 'c', long = "connections", default_value_t = 8)]
        max_connections: usize,
        /// Extra request header, "Name: value". Repeatable.
        #[arg(short = 'H', long = "header")]
        headers: Vec<String>,
        /// Output file name.
        #[arg(short = 'o', long)]
        output: Option<String>,
        /// Print one Snapshot JSON line per second instead of the live bar.
        #[arg(long)]
        json: bool,
    },
}

const PALETTE: [(u8, u8, u8); 4] = [(0x0C, 0x7A, 0x64), (0x2F, 0x6F, 0xA8), (0xC9, 0x83, 0x1F), (0x7B, 0x5B, 0xA6)];
const BAR_WIDTH: usize = 60;

#[tokio::main]
async fn main() {
    let code = match real_main().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("spate: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

async fn real_main() -> Result<i32> {
    let Cmd::Get { url, dir, max_connections, headers, output, json } = Cli::parse().cmd;
    let mut hdrs = Vec::new();
    for h in headers {
        let Some((k, v)) = h.split_once(':') else { bail!("bad header {h:?}, expected \"Name: value\"") };
        hdrs.push((k.trim().to_string(), v.trim().to_string()));
    }
    let dir = dir.unwrap_or_else(spate_proto::default_download_dir);
    std::fs::create_dir_all(&dir)?;
    let job = Job { url, headers: hdrs, dir, filename: output, max_connections: max_connections.max(1) };

    let progress = Arc::new(Mutex::new(Snapshot::default()));
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = cancel_tx.send(true);
        }
    });

    let mut handle = tokio::spawn(spate_core::run(job, progress.clone(), cancel_rx));
    let mut ui = Ui { json, drawn: false };
    let period = if json { Duration::from_secs(1) } else { Duration::from_millis(100) };
    let mut tick = tokio::time::interval(period);
    let result = loop {
        tokio::select! {
            r = &mut handle => break r?,
            _ = tick.tick() => ui.draw(&snap(&progress)),
        }
    };
    ui.draw(&snap(&progress));
    ui.finish();
    match result {
        Ok(path) => {
            println!("{}", path.display());
            Ok(0)
        }
        Err(e) if e.is::<spate_core::Paused>() => {
            eprintln!("Paused. Run the same command again to resume.");
            Ok(130)
        }
        Err(e) => Err(e),
    }
}

fn snap(p: &Arc<Mutex<Snapshot>>) -> Snapshot {
    p.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

struct Ui {
    json: bool,
    drawn: bool,
}

impl Ui {
    fn draw(&mut self, s: &Snapshot) {
        if self.json {
            if let Ok(line) = serde_json::to_string(s) {
                println!("{line}");
            }
            return;
        }
        let mut out = std::io::stderr().lock();
        let mut frame = String::new();
        if self.drawn {
            frame.push_str("\x1b[2F"); // up two lines
        } else {
            frame.push_str("\x1b[?25l"); // hide cursor
        }
        frame.push_str("\x1b[2K");
        frame.push_str(&status_line(s));
        frame.push_str("\n\x1b[2K");
        frame.push_str(&bar(s));
        frame.push_str("\x1b[0m\n");
        let _ = out.write_all(frame.as_bytes());
        let _ = out.flush();
        self.drawn = true;
    }

    fn finish(&mut self) {
        if !self.json && self.drawn {
            let _ = std::io::stderr().write_all(b"\x1b[?25h");
        }
    }
}

fn status_line(s: &Snapshot) -> String {
    let total = s.total.map(human).unwrap_or_else(|| "?".into());
    let eta = match s.total {
        Some(t) if s.speed_bps > 1.0 && t > s.downloaded => {
            format!("ETA {}", duration((t - s.downloaded) as f64 / s.speed_bps))
        }
        Some(_) if s.done => "done".into(),
        _ => "ETA --".into(),
    };
    let name = if s.filename.is_empty() { "…" } else { &s.filename };
    format!(
        "{name}  {}/{total}  {}/s  {eta}  {} conn",
        human(s.downloaded),
        human(s.speed_bps as u64),
        s.connections
    )
}

/// One colour per segment: solid block where received, dim shade where still to come.
fn bar(s: &Snapshot) -> String {
    let Some(total) = s.total.filter(|t| *t > 0) else {
        return "\x1b[2m(size unknown)".into();
    };
    let mut out = String::new();
    let mut last: Option<(usize, bool)> = None;
    for col in 0..BAR_WIDTH {
        let mid = ((col as f64 + 0.5) * total as f64 / BAR_WIDTH as f64) as u64;
        let idx = s.segments.iter().rposition(|g| g.start <= mid);
        let (ci, got) = match idx {
            Some(i) => (i % PALETTE.len(), s.segments[i].pos > mid && mid < s.segments[i].end.max(s.segments[i].pos)),
            None => (0, false),
        };
        if last != Some((ci, got)) {
            let (r, g, b) = PALETTE[ci];
            if got {
                out.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
            } else {
                let d = |c: u8| (c as u16 * 45 / 100) as u8;
                out.push_str(&format!("\x1b[38;2;{};{};{}m", d(r), d(g), d(b)));
            }
            last = Some((ci, got));
        }
        out.push(if got { '█' } else { '░' });
    }
    let pct = (s.downloaded as f64 / total as f64 * 100.0).min(100.0);
    out.push_str(&format!("\x1b[0m {pct:5.1}%"));
    out
}

fn human(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn duration(secs: f64) -> String {
    let s = secs.round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
