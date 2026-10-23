//! `spate` command-line client.
//!
//! `spate get <URL>` downloads in-process (no daemon). The other subcommands talk to the
//! daemon over its Unix socket (CONTRACTS.md, "Daemon IPC").

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use spate_proto::{Job, JobEvent, JobState, JobView, SegmentView, Snapshot};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Parser)]
#[command(name = "spate", version, about = "Spate download accelerator")]
struct Cli {
    /// Daemon socket (default ~/Library/Application Support/Spate/spate.sock).
    #[arg(long, global = true, env = "SPATE_SOCKET")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download a URL in this process, without the daemon (Ctrl-C pauses; run the same
    /// command again to resume).
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
    /// Queue a download in the daemon.
    Add {
        url: String,
        /// Extra request header, "Name: value". Repeatable.
        #[arg(short = 'H', long = "header")]
        headers: Vec<String>,
        /// Output file name.
        #[arg(short = 'o', long)]
        output: Option<String>,
    },
    /// List the daemon's downloads.
    Ls,
    /// Pause a download (ID or unique ID prefix).
    Pause { id: String },
    /// Resume a paused or failed download.
    Resume { id: String },
    /// Stop a download and delete its partial file.
    Cancel { id: String },
    /// Remove a finished or failed download from the list (the file stays).
    Rm { id: String },
    /// Live view of all downloads.
    Watch {
        /// Exit once nothing is queued or running.
        #[arg(long)]
        until_idle: bool,
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
    let cli = Cli::parse();
    let socket = cli.socket.unwrap_or_else(spate_proto::socket_path);
    match cli.cmd {
        Cmd::Get { url, dir, max_connections, headers, output, json } => {
            get(url, dir, max_connections, parse_headers(headers)?, output, json).await
        }
        Cmd::Add { url, headers, output } => {
            let mut c = Client::connect(&socket).await?;
            let params = json!({"url": url, "headers": parse_headers(headers)?, "filename": output,
                                "source": "cli", "kind": "file"});
            let r = c.call("add", params).await?;
            println!("{}", r["job_id"].as_str().unwrap_or_default());
            Ok(0)
        }
        Cmd::Ls => {
            let mut c = Client::connect(&socket).await?;
            let jobs = c.list().await?;
            print!("{}", table(&jobs));
            Ok(0)
        }
        Cmd::Pause { id } => job_cmd(&socket, "pause", &id).await,
        Cmd::Resume { id } => job_cmd(&socket, "resume", &id).await,
        Cmd::Cancel { id } => job_cmd(&socket, "cancel", &id).await,
        Cmd::Rm { id } => job_cmd(&socket, "remove", &id).await,
        Cmd::Watch { until_idle } => watch(&socket, until_idle).await,
    }
}

/// "Name: value" strings → pairs; adds `User-Agent: Spate/<version>` unless one was given.
fn parse_headers(raw: Vec<String>) -> Result<Vec<(String, String)>> {
    let mut hdrs = Vec::new();
    for h in raw {
        let Some((k, v)) = h.split_once(':') else { bail!("bad header {h:?}, expected \"Name: value\"") };
        hdrs.push((k.trim().to_string(), v.trim().to_string()));
    }
    if !hdrs.iter().any(|(k, _)| k.eq_ignore_ascii_case("user-agent")) {
        hdrs.push(("User-Agent".into(), spate_core::SPATE_USER_AGENT.into()));
    }
    Ok(hdrs)
}

// ---------------------------------------------------------------------------------------------
// spate get

async fn get(
    url: String,
    dir: Option<PathBuf>,
    max_connections: usize,
    headers: Vec<(String, String)>,
    output: Option<String>,
    json: bool,
) -> Result<i32> {
    let dir = dir.unwrap_or_else(spate_proto::default_download_dir);
    std::fs::create_dir_all(&dir)?;
    let job =
        Job { url, headers, dir, filename: output, max_connections: max_connections.max(1), start_connections: None };

    let progress = Arc::new(Mutex::new(Snapshot::default()));
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = cancel_tx.send(true);
        }
    });

    let mut handle = tokio::spawn(spate_core::run(job, progress.clone(), cancel_rx));
    let tty = std::io::stderr().is_terminal();
    let mut ui = Ui { json, tty, drawn: 0, last_plain: None };
    let period = if json || !tty { Duration::from_secs(1) } else { Duration::from_millis(100) };
    let mut tick = tokio::time::interval(period);
    let result = loop {
        tokio::select! {
            r = &mut handle => break r?,
            _ = tick.tick() => ui.draw_snapshot(&snap(&progress)),
        }
    };
    ui.draw_snapshot(&snap(&progress));
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

/// Live output on stderr. With a terminal: redrawn in place with coloured bars. Without one
/// (pipe, log file): plain lines, no escape sequences.
struct Ui {
    json: bool,
    tty: bool,
    /// Lines drawn by the previous frame (to move back up over them).
    drawn: usize,
    last_plain: Option<Instant>,
}

impl Ui {
    fn draw_snapshot(&mut self, s: &Snapshot) {
        if self.json {
            if let Ok(line) = serde_json::to_string(s) {
                println!("{line}");
            }
            return;
        }
        let status = status_line(&s.filename, s.total, s.downloaded, s.speed_bps, s.connections, s.done);
        if !self.tty {
            eprintln!("{status}");
            return;
        }
        let lines = vec![status, bar(s.total, &s.segments, s.downloaded, true)];
        self.frame(&lines);
    }

    /// Replace the previous frame with `lines` (terminal only).
    fn frame(&mut self, lines: &[String]) {
        let mut out = String::new();
        if self.drawn > 0 {
            out.push_str(&format!("\x1b[{}F", self.drawn)); // up to the frame's first line
        } else {
            out.push_str("\x1b[?25l"); // hide cursor
        }
        out.push_str("\x1b[J"); // clear to the end of the screen
        for l in lines {
            out.push_str(l);
            out.push_str("\x1b[0m\n");
        }
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(out.as_bytes());
        let _ = err.flush();
        self.drawn = lines.len();
    }

    fn finish(&mut self) {
        if self.tty && !self.json && self.drawn > 0 {
            let _ = std::io::stderr().write_all(b"\x1b[?25h");
        }
    }
}

fn status_line(name: &str, total: Option<u64>, downloaded: u64, speed: f64, conns: usize, done: bool) -> String {
    let total_s = total.map(human).unwrap_or_else(|| "?".into());
    let eta = match total {
        Some(t) if speed > 1.0 && t > downloaded => format!("ETA {}", duration((t - downloaded) as f64 / speed)),
        Some(_) if done => "done".into(),
        _ => "ETA --".into(),
    };
    let name = if name.is_empty() { "…" } else { name };
    format!("{name}  {}/{total_s}  {}/s  {eta}  {conns} conn", human(downloaded), human(speed as u64))
}

/// One colour per segment: solid block where received, dim shade where still to come.
fn bar(total: Option<u64>, segments: &[SegmentView], downloaded: u64, color: bool) -> String {
    let Some(total) = total.filter(|t| *t > 0) else {
        return if color { "\x1b[2m(size unknown)".into() } else { "(size unknown)".into() };
    };
    let mut out = String::new();
    let mut last: Option<(usize, bool)> = None;
    for col in 0..BAR_WIDTH {
        let mid = ((col as f64 + 0.5) * total as f64 / BAR_WIDTH as f64) as u64;
        let idx = segments.iter().rposition(|g| g.start <= mid);
        let (ci, got) = match idx {
            Some(i) => (i % PALETTE.len(), segments[i].pos > mid && mid < segments[i].end.max(segments[i].pos)),
            // No segments yet (queued) or all done.
            None => (0, downloaded >= total),
        };
        if color && last != Some((ci, got)) {
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
    let pct = (downloaded as f64 / total as f64 * 100.0).min(100.0);
    if color {
        out.push_str("\x1b[0m");
    }
    out.push_str(&format!(" {pct:5.1}%"));
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

// ---------------------------------------------------------------------------------------------
// Daemon client

struct Client {
    r: BufReader<tokio::net::unix::OwnedReadHalf>,
    w: tokio::net::unix::OwnedWriteHalf,
    id: u64,
}

impl Client {
    async fn connect(path: &std::path::Path) -> Result<Client> {
        let s = UnixStream::connect(path).await.map_err(|e| {
            anyhow!("cannot reach the Spate daemon at {} ({e}); is spate-daemon or the app running?", path.display())
        })?;
        let (r, w) = s.into_split();
        Ok(Client { r: BufReader::new(r), w, id: 0 })
    }

    async fn read_line(&mut self) -> Result<Value> {
        let mut s = String::new();
        if self.r.read_line(&mut s).await? == 0 {
            bail!("the daemon closed the connection");
        }
        serde_json::from_str(s.trim()).context("bad reply from the daemon")
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.id += 1;
        let mut req = serde_json::to_vec(&json!({"id": self.id, "method": method, "params": params}))?;
        req.push(b'\n');
        self.w.write_all(&req).await?;
        loop {
            let v = self.read_line().await?;
            if v.get("event").is_some() {
                continue; // a push on a subscribed connection
            }
            if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
                bail!("{}: {}", e["code"].as_str().unwrap_or("error"), e["message"].as_str().unwrap_or(""));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn list(&mut self) -> Result<Vec<JobView>> {
        Ok(serde_json::from_value(self.call("list", Value::Null).await?)?)
    }
}

/// Resolve an ID prefix against the daemon's list.
fn resolve<'a>(jobs: &'a [JobView], prefix: &str) -> Result<&'a JobView> {
    let m: Vec<&JobView> = jobs.iter().filter(|j| j.id.starts_with(prefix)).collect();
    match m.as_slice() {
        [one] => Ok(one),
        [] => bail!("no download with id {prefix}"),
        _ => bail!("id {prefix} is ambiguous ({} matches)", m.len()),
    }
}

async fn job_cmd(socket: &std::path::Path, method: &str, prefix: &str) -> Result<i32> {
    let mut c = Client::connect(socket).await?;
    let jobs = c.list().await?;
    let job = resolve(&jobs, prefix)?;
    c.call(method, json!({"job_id": job.id})).await?;
    let verb = match method {
        "pause" => "paused",
        "resume" => "resumed",
        "cancel" => "cancelled",
        _ => "removed",
    };
    println!("{verb} {} {}", short(&job.id), job.filename);
    Ok(0)
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn state_name(s: JobState) -> &'static str {
    match s {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Paused => "paused",
        JobState::Done => "done",
        JobState::Failed => "failed",
    }
}

fn percent(j: &JobView) -> String {
    match j.total {
        Some(t) if t > 0 => format!("{:.1}%", (j.downloaded as f64 / t as f64 * 100.0).min(100.0)),
        _ if j.state == JobState::Done => "100.0%".into(),
        _ => "-".into(),
    }
}

fn speed(j: &JobView) -> String {
    if j.state == JobState::Running {
        format!("{}/s", human(j.speed_bps as u64))
    } else {
        "-".into()
    }
}

fn table(jobs: &[JobView]) -> String {
    let rows: Vec<[String; 5]> = jobs
        .iter()
        .map(|j| {
            let mut state = state_name(j.state).to_string();
            if let Some(e) = &j.error {
                if j.state == JobState::Failed {
                    state = format!("failed ({})", e.split(':').next().unwrap_or(e));
                }
            }
            [short(&j.id).to_string(), j.filename.clone(), state, percent(j), speed(j)]
        })
        .collect();
    let head = ["ID", "NAME", "STATE", "PROGRESS", "SPEED"];
    let mut w: Vec<usize> = head.iter().map(|h| h.chars().count()).collect();
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    w[1] = w[1].min(48);
    let fmt = |cells: [&str; 5]| {
        let mut line = String::new();
        for (i, c) in cells.iter().enumerate() {
            let c: String = if c.chars().count() > w[i] { c.chars().take(w[i] - 1).chain(['…']).collect() } else { c.to_string() };
            if i == 3 || i == 4 {
                line.push_str(&format!("{c:>width$}  ", width = w[i]));
            } else {
                line.push_str(&format!("{c:<width$}  ", width = w[i]));
            }
        }
        line.trim_end().to_string() + "\n"
    };
    let mut out = fmt(head);
    for r in &rows {
        out.push_str(&fmt([&r[0], &r[1], &r[2], &r[3], &r[4]]));
    }
    if rows.is_empty() {
        out.push_str("(no downloads)\n");
    }
    out
}

// ---------------------------------------------------------------------------------------------
// spate watch

async fn watch(socket: &std::path::Path, until_idle: bool) -> Result<i32> {
    let mut c = Client::connect(socket).await?;
    c.call("subscribe", Value::Null).await?;
    // The daemon pushes every job's current state first; seed from `list` on a second
    // connection so an empty daemon is recognised right away.
    let mut jobs: BTreeMap<String, JobView> = BTreeMap::new();
    for j in Client::connect(socket).await?.list().await? {
        jobs.insert(j.id.clone(), j);
    }
    let tty = std::io::stderr().is_terminal();
    let mut ui = Ui { json: false, tty, drawn: 0, last_plain: None };
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut last_states: BTreeMap<String, JobState> = BTreeMap::new();
    let idle = |jobs: &BTreeMap<String, JobView>| {
        jobs.values().all(|j| !matches!(j.state, JobState::Queued | JobState::Running))
    };
    let code = loop {
        if until_idle && idle(&jobs) {
            break 0;
        }
        let next = tokio::select! {
            v = c.read_line() => v,
            _ = tokio::signal::ctrl_c() => break 130,
        };
        let v = next?;
        let Ok(ev) = serde_json::from_value::<JobEvent>(v) else { continue };
        if ev.event == "removed" {
            jobs.remove(&ev.job.id);
        } else {
            jobs.insert(ev.job.id.clone(), ev.job);
        }
        if tty {
            if last_draw.elapsed() >= Duration::from_millis(100) {
                ui.frame(&watch_lines(&jobs));
                last_draw = Instant::now();
            }
        } else {
            watch_plain(&jobs, &mut last_states, &mut ui.last_plain);
        }
    };
    if tty {
        ui.frame(&watch_lines(&jobs));
        ui.finish();
    } else {
        ui.last_plain = None;
        watch_plain(&jobs, &mut last_states, &mut ui.last_plain);
    }
    Ok(code)
}

fn ordered(jobs: &BTreeMap<String, JobView>) -> Vec<&JobView> {
    let mut v: Vec<&JobView> = jobs.values().collect();
    v.sort_by_key(|j| j.created_at);
    v
}

/// Terminal frame: a status line per job and a segment bar under the active ones.
fn watch_lines(jobs: &BTreeMap<String, JobView>) -> Vec<String> {
    let mut lines = Vec::new();
    for j in ordered(jobs) {
        let state = state_name(j.state);
        let status = status_line(&j.filename, j.total, j.downloaded, j.speed_bps, j.connections, j.state == JobState::Done);
        let color = match j.state {
            JobState::Running => "\x1b[1m",
            JobState::Failed => "\x1b[31m",
            JobState::Done => "\x1b[32m",
            _ => "\x1b[2m",
        };
        let mut line = format!("{color}{} {state:<7}\x1b[0m {status}", short(&j.id));
        if let (JobState::Failed, Some(e)) = (j.state, &j.error) {
            line.push_str(&format!("  \x1b[31m{e}"));
        }
        lines.push(line);
        if matches!(j.state, JobState::Running | JobState::Paused) {
            lines.push(format!("         {}", bar(j.total, &j.segments, j.downloaded, true)));
        }
    }
    if lines.is_empty() {
        lines.push("(no downloads)".into());
    }
    lines
}

/// Pipe/log output: a line on every state change, progress of running jobs every 2 s.
fn watch_plain(jobs: &BTreeMap<String, JobView>, last: &mut BTreeMap<String, JobState>, last_progress: &mut Option<Instant>) {
    let progress_due = last_progress.is_none_or(|t| t.elapsed() >= Duration::from_secs(2));
    let mut printed = false;
    for j in ordered(jobs) {
        let changed = last.get(&j.id) != Some(&j.state);
        if changed || (progress_due && j.state == JobState::Running) {
            let mut line = format!(
                "{} {:<7} {}",
                short(&j.id),
                state_name(j.state),
                status_line(&j.filename, j.total, j.downloaded, j.speed_bps, j.connections, j.state == JobState::Done)
            );
            if let Some(e) = &j.error {
                line.push_str(&format!("  ({e})"));
            }
            if let Some(p) = j.path.as_ref().filter(|_| j.state == JobState::Done) {
                line.push_str(&format!("  -> {}", p.display()));
            }
            eprintln!("{line}");
            printed = true;
        }
        last.insert(j.id.clone(), j.state);
    }
    if progress_due && printed {
        *last_progress = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(id: &str, state: JobState, total: Option<u64>, downloaded: u64) -> JobView {
        JobView {
            id: id.into(),
            url: "https://x/a.bin".into(),
            filename: "a.bin".into(),
            path: None,
            state,
            total,
            downloaded,
            speed_bps: 2_000_000.0,
            eta_secs: None,
            connections: 4,
            segments: vec![],
            error: None,
            created_at: 0,
            finished_at: None,
        }
    }

    #[test]
    fn prefix_resolution() {
        let jobs = vec![view("abcd-1", JobState::Done, None, 0), view("abce-2", JobState::Done, None, 0)];
        assert_eq!(resolve(&jobs, "abcd").unwrap().id, "abcd-1");
        assert!(resolve(&jobs, "abc").unwrap_err().to_string().contains("ambiguous"));
        assert!(resolve(&jobs, "zz").is_err());
    }

    #[test]
    fn ls_table() {
        let jobs = vec![
            view("11111111-aaaa", JobState::Running, Some(200), 50),
            view("22222222-bbbb", JobState::Queued, None, 0),
        ];
        let t = table(&jobs);
        println!("{t}");
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].starts_with("ID"));
        assert!(lines[1].starts_with("11111111  a.bin  running") && lines[1].contains("25.0%") && lines[1].contains("1.9 MB/s"));
        assert!(lines[2].contains("queued") && lines[2].ends_with('-'));
    }

    #[test]
    fn plain_bar_has_no_escapes() {
        let segs = vec![SegmentView { start: 0, end: 100, pos: 50, active: true }];
        let b = bar(Some(100), &segs, 50, false);
        assert!(!b.contains('\x1b'));
        assert!(b.contains("50.0%"));
        assert!(bar(Some(100), &segs, 50, true).contains('\x1b'));
    }

    #[test]
    fn default_user_agent() {
        let h = parse_headers(vec![]).unwrap();
        assert_eq!(h, vec![("User-Agent".to_string(), spate_core::SPATE_USER_AGENT.to_string())]);
        let h = parse_headers(vec!["user-agent: curl/8".into()]).unwrap();
        assert_eq!(h, vec![("user-agent".to_string(), "curl/8".to_string())]);
        assert!(parse_headers(vec!["nocolon".into()]).is_err());
    }
}
