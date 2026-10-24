//! Spate.app: the Tauri 2 shell. Runs the daemon in-process (`spate_daemon::Daemon`), serves
//! the web UI in `app/ui/`, and owns the menu-bar icon, the capture cards and the notch island.
//!
//! Commands and events are the ones `app/ui/js/api.js` expects: `list`, `add`, `pause`,
//! `resume`, `cancel`, `remove`, `reveal`, `open`, `open_window`, `choose_folder`, `set_island`;
//! events `job` (JobView) and `removed` (job id) to every window.

mod commands;
mod macos;
mod util;
mod windows;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spate_daemon::log::Logger;
use spate_daemon::{Config, Daemon, DaemonEvent, Handle};
use spate_proto::{JobState, JobView, Source};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{ActivationPolicy, AppHandle, Emitter, Manager, RunEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::broadcast::error::RecvError;

use windows::{IslandMode, IslandState};

/// Everything the shell keeps besides the daemon itself.
pub struct Shell {
    pub daemon: Handle,
    pub log: Logger,
    pub data_dir: PathBuf,
    /// Folder chosen on the capture card, applied when the job finishes.
    pub dest: Mutex<HashMap<String, PathBuf>>,
    pub island: Mutex<IslandState>,
    pub popover_hidden_at: Mutex<Option<Instant>>,
    /// Notification id → job id, so a click can reveal the file.
    notes: Mutex<HashMap<i32, String>>,
    next_note: AtomicI32,
    forwarded: AtomicU64,
    tray: Mutex<Option<TrayIcon>>,
}

impl Shell {
    pub fn job(&self, id: &str) -> Option<JobView> {
        self.daemon.get(id)
    }
}

pub fn log(app: &AppHandle, msg: &str) {
    if let Some(s) = app.try_state::<Arc<Shell>>() {
        s.log.log(msg);
    }
    eprintln!("spate-app: {msg}");
}

fn app_log_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    home.join("Library/Logs/Spate/app.log")
}

fn main() {
    let app = tauri::Builder::default()
        // Must come first: a second copy hands over and quits before doing anything else.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            log(app, "second instance started: focusing this one");
            windows::open_main(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .macos_launcher(tauri_plugin_autostart::MacosLauncher::LaunchAgent)
                .app_name("Spate")
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            commands::list,
            commands::add,
            commands::pause,
            commands::resume,
            commands::cancel,
            commands::remove,
            commands::reveal,
            commands::open,
            commands::open_window,
            commands::choose_folder,
            commands::set_island,
        ])
        .setup(|app| {
            app.set_activation_policy(ActivationPolicy::Accessory);
            let handle = app.handle().clone();
            let logger = Logger::new(Some(app_log_path()));
            logger.log(&format!("Spate app v{} starting", spate_proto::VERSION));
            let config = Config::default();
            let data_dir = config.data_dir.clone();
            let socket = config.socket_path.clone();
            let daemon = match tauri::async_runtime::block_on(Daemon::start(config)) {
                Ok(d) => d,
                Err(e) => {
                    let msg = format!("{e:#}");
                    logger.log(&format!("daemon failed to start: {msg}"));
                    eprintln!("spate-app: daemon failed to start: {msg}");
                    let text = if msg.contains("already running") {
                        format!(
                            "Another Spate is already running: a daemon answers on\n{}\n\nQuit it (or stop `spate-daemon`) and open Spate again.",
                            socket.map(|p| p.display().to_string()).unwrap_or_default()
                        )
                    } else {
                        format!("Spate could not start its download engine:\n\n{msg}")
                    };
                    // A menu-bar agent's alert would open behind the frontmost app. setup() runs
                    // on the main thread, so the alert can block right here.
                    app.set_activation_policy(ActivationPolicy::Regular);
                    macos::activate_app();
                    macos::alert("Spate can't start", &text);
                    std::process::exit(1);
                }
            };
            // Subscribe before anything can happen, and know the history, so only new jobs
            // count as "new".
            let events = daemon.subscribe();
            let known: HashMap<String, JobState> = daemon.list().into_iter().map(|v| (v.id, v.state)).collect();
            let shell = Arc::new(Shell {
                daemon: daemon.clone(),
                log: logger,
                data_dir,
                dest: Mutex::new(HashMap::new()),
                island: Mutex::new(IslandState::default()),
                popover_hidden_at: Mutex::new(None),
                notes: Mutex::new(HashMap::new()),
                next_note: AtomicI32::new(1),
                forwarded: AtomicU64::new(0),
                tray: Mutex::new(None),
            });
            app.manage(shell.clone());
            tauri::async_runtime::block_on(migrate_moves(&shell));

            windows::create_all(&handle)?;
            build_tray(&handle)?;
            setup_login_item(&handle);
            setup_notification_clicks(&handle);

            tauri::async_runtime::spawn(forward(handle.clone(), shell.clone(), events, known));
            tauri::async_runtime::spawn(ticker(handle.clone(), shell.clone()));
            tauri::async_runtime::spawn(island_mouse(handle.clone(), shell.clone()));
            tauri::async_runtime::spawn(quit_on_signal(handle.clone(), shell.clone()));
            shell.log.log("shell ready: windows, tray and event forwarding up");
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Spate");

    app.run(|app, event| match event {
        RunEvent::ExitRequested { code: None, api, .. } => {
            // Closing the last window (a card) doesn't quit a menu-bar app.
            api.prevent_exit();
        }
        RunEvent::ExitRequested { .. } => {
            // Panels go back to their own class before any teardown (see macos::unmake_panel).
            for w in app.webview_windows().values() {
                macos::unmake_panel(w);
            }
        }
        RunEvent::Exit => {
            for w in app.webview_windows().values() {
                macos::unmake_panel(w);
            }
            if let Some(s) = app.try_state::<Arc<Shell>>() {
                s.log.log("exiting: stopping the daemon");
                tauri::async_runtime::block_on(s.daemon.shutdown());
            }
        }
        _ => {}
    });
}

// ---------------------------------------------------------------------------------------------
// Menu-bar icon.

const TRAY_ICON: &[u8] = include_bytes!("../icons/tray.png");

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let login = CheckMenuItem::with_id(app, "login", "Start at Login", true, false, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "open", "Open Spate", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "pause_all", "Pause All", true, None::<&str>)?,
            &MenuItem::with_id(app, "resume_all", "Resume All", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &login,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "quit", "Quit Spate", true, Some("Cmd+Q"))?,
        ],
    )?;
    app.manage(login);
    let tray = TrayIconBuilder::with_id("spate")
        .icon(tauri::image::Image::from_bytes(TRAY_ICON)?)
        .icon_as_template(true)
        .tooltip("Spate")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                windows::toggle_popover(tray.app_handle());
            }
        })
        .on_menu_event(|app, ev| {
            let shell = app.state::<Arc<Shell>>().inner().clone();
            match ev.id().as_ref() {
                "open" => windows::open_main(app),
                "pause_all" => {
                    tauri::async_runtime::spawn(async move {
                        for j in shell.daemon.list() {
                            if matches!(j.state, JobState::Running | JobState::Queued) {
                                let _ = shell.daemon.pause(&j.id).await;
                            }
                        }
                    });
                }
                "resume_all" => {
                    tauri::async_runtime::spawn(async move {
                        for j in shell.daemon.list() {
                            if j.state == JobState::Paused {
                                let _ = shell.daemon.resume(&j.id).await;
                            }
                        }
                    });
                }
                "login" => toggle_login_item(app),
                "quit" => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        shell.log.log("quit from the menu");
                        shell.daemon.shutdown().await;
                        app.exit(0);
                    });
                }
                _ => {}
            }
        })
        .build(app)?;
    *app.state::<Arc<Shell>>().tray.lock().unwrap() = Some(tray);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Start at login (a LaunchAgent in ~/Library/LaunchAgents): on after the first launch, then
// whatever the menu says.

fn setup_login_item(app: &AppHandle) {
    let shell = app.state::<Arc<Shell>>();
    let marker = shell.data_dir.join(".login-item-initialised");
    let al = app.autolaunch();
    if !marker.exists() {
        match al.enable() {
            Ok(()) => shell.log.log("start at login: enabled (first launch)"),
            Err(e) => shell.log.log(&format!("start at login: could not enable: {e}")),
        }
        let _ = std::fs::write(&marker, b"");
    }
    let on = al.is_enabled().unwrap_or(false);
    app.state::<CheckMenuItem<tauri::Wry>>().set_checked(on).ok();
}

fn toggle_login_item(app: &AppHandle) {
    let al = app.autolaunch();
    let on = al.is_enabled().unwrap_or(false);
    let r = if on { al.disable() } else { al.enable() };
    if let Err(e) = r {
        log(app, &format!("start at login: toggle failed: {e}"));
    }
    let now = al.is_enabled().unwrap_or(false);
    log(app, &format!("start at login: {}", if now { "on" } else { "off" }));
    app.state::<CheckMenuItem<tauri::Wry>>().set_checked(now).ok();
}

// ---------------------------------------------------------------------------------------------
// Notifications.

fn setup_notification_clicks(app: &AppHandle) {
    let h = app.clone();
    let _ = app.notification().on_action(move |a| {
        if a.action_id() != "tap" {
            return;
        }
        let Some(nid) = a.notification().map(|n| n.id()) else { return };
        let shell = h.state::<Arc<Shell>>();
        let job = shell.notes.lock().unwrap().get(&nid).cloned();
        if let Some(id) = job {
            let _ = commands::reveal_job(&shell, &id);
        }
    });
}

fn notify(app: &AppHandle, shell: &Shell, job: &JobView) {
    let (title, body) = match job.state {
        JobState::Done => ("Download finished", job.filename.clone()),
        _ => ("Download failed", format!("{} — {}", job.filename, job.error.as_deref().unwrap_or("failed"))),
    };
    let nid = shell.next_note.fetch_add(1, Ordering::Relaxed);
    shell.notes.lock().unwrap().insert(nid, job.id.clone());
    if let Err(e) = app.notification().builder().id(nid).title(title).body(body).show() {
        shell.log.log(&format!("notification failed: {e}"));
    }
}

// ---------------------------------------------------------------------------------------------
// Daemon events → windows.

async fn forward(
    app: AppHandle,
    shell: Arc<Shell>,
    mut rx: tokio::sync::broadcast::Receiver<DaemonEvent>,
    mut last: HashMap<String, JobState>,
) {
    loop {
        match rx.recv().await {
            Ok(DaemonEvent::Job(v)) => {
                shell.forwarded.fetch_add(1, Ordering::Relaxed);
                let prev = last.insert(v.id.clone(), v.state);
                if prev != Some(v.state) {
                    shell.log.log(&format!(
                        "event job {} {} -> {:?} (source {:?}) forwarded",
                        v.id,
                        prev.map(|p| format!("{p:?}")).unwrap_or_else(|| "new".into()),
                        v.state,
                        v.source
                    ));
                }
                let fresh = prev.is_none();
                if fresh && v.source == Some(Source::Brave) && !matches!(v.state, JobState::Done | JobState::Failed) {
                    windows::show_card(&app, &v.id);
                }
                let finished = v.state == JobState::Done && prev != Some(JobState::Done);
                let failed = v.state == JobState::Failed
                    && prev.is_some_and(|p| p != JobState::Failed)
                    && v.error.as_deref() != Some("cancelled");
                if finished {
                    let dest = shell.dest.lock().unwrap().remove(&v.id);
                    if let Some(dir) = dest {
                        // Move off this loop (a move across volumes copies the file).
                        let (app, shell) = (app.clone(), shell.clone());
                        tauri::async_runtime::spawn(async move {
                            let v = move_finished(&shell, v, dir).await;
                            let _ = app.emit("job", &v);
                            notify(&app, &shell, &v);
                        });
                        continue;
                    }
                }
                let _ = app.emit("job", &v);
                if finished || failed {
                    notify(&app, &shell, &v);
                }
            }
            Ok(DaemonEvent::Removed(id)) => {
                last.remove(&id);
                shell.dest.lock().unwrap().remove(&id);
                shell.log.log(&format!("event removed {id} forwarded"));
                let _ = app.emit("removed", &id);
            }
            Err(RecvError::Lagged(n)) => {
                shell.log.log(&format!("event stream lagged by {n}; resending every job"));
                for v in shell.daemon.list() {
                    last.insert(v.id.clone(), v.state);
                    let _ = app.emit("job", &v);
                }
            }
            Err(RecvError::Closed) => break,
        }
    }
}

/// Move a finished file into the folder chosen on its card and record the new path in the
/// daemon (`set_path`). Returns the job to show.
pub async fn move_finished(shell: &Arc<Shell>, v: JobView, dir: PathBuf) -> JobView {
    let Some(path) = v.path.clone() else { return v };
    let target = dir.clone();
    let res = tauri::async_runtime::spawn_blocking(move || util::move_into(&path, &target)).await;
    match res {
        Ok(Ok(p)) => {
            shell.log.log(&format!("job {} moved to the chosen folder", v.id));
            if let Err(e) = shell.daemon.set_path(&v.id, &p).await {
                shell.log.log(&format!("job {}: set_path failed: {}: {}", v.id, e.code, e.message));
            }
        }
        Ok(Err(e)) => shell.log.log(&format!("job {}: move to chosen folder failed: {e}", v.id)),
        Err(e) => shell.log.log(&format!("job {}: move task failed: {e}", v.id)),
    }
    shell.daemon.get(&v.id).unwrap_or(v)
}

/// Versions before `set_path` kept the moved paths in `moved-files.json`. Hand them to the
/// daemon once and drop the file. Entries that can't be recorded (file gone, job removed) are
/// logged and dropped: the daemon's history is the only record from now on.
async fn migrate_moves(shell: &Shell) {
    let file = shell.data_dir.join("moved-files.json");
    if !file.exists() {
        return;
    }
    let (mut ok, mut dropped) = (0, 0);
    for (id, path) in util::load_legacy_moves(&file) {
        if shell.daemon.get(&id).and_then(|v| v.path).is_some_and(|p| p == path) {
            ok += 1;
            continue;
        }
        match shell.daemon.set_path(&id, &path).await {
            Ok(()) => ok += 1,
            Err(e) => {
                dropped += 1;
                shell.log.log(&format!("moved-files.json: job {id} not migrated: {}: {}", e.code, e.message));
            }
        }
    }
    match std::fs::remove_file(&file) {
        Ok(()) => shell.log.log(&format!("moved-files.json migrated ({ok} recorded, {dropped} dropped) and removed")),
        Err(e) => shell.log.log(&format!("moved-files.json: could not remove: {e}")),
    }
}

/// Twice a second: the menu-bar speed and whether the island should be up.
async fn ticker(app: AppHandle, shell: Arc<Shell>) {
    let mut title: Option<String> = None;
    let mut last_report = Instant::now();
    let mut reported = 0u64;
    let mut iv = tokio::time::interval(Duration::from_millis(500));
    loop {
        iv.tick().await;
        let jobs = shell.daemon.list();
        let running: Vec<&JobView> = jobs.iter().filter(|j| j.state == JobState::Running).collect();
        let total: f64 = running.iter().map(|j| j.speed_bps).sum();
        let want = (!running.is_empty()).then(|| util::speed_short(total));
        if want != title {
            title = want.clone();
            if let Some(t) = shell.tray.lock().unwrap().as_ref() {
                let _ = t.set_title(want.as_deref());
            }
        }
        // The island is up exactly while something downloads.
        let shown = shell.island.lock().unwrap().shown;
        if running.is_empty() == shown {
            let app2 = app.clone();
            let up = !running.is_empty();
            let _ = app.run_on_main_thread(move || {
                if let Some(w) = app2.get_webview_window(windows::ISLAND) {
                    if up {
                        windows::show_island(&app2, &w);
                    } else {
                        windows::hide_island(&app2, &w);
                    }
                }
            });
        }
        if last_report.elapsed() >= Duration::from_secs(10) {
            let n = shell.forwarded.load(Ordering::Relaxed);
            if n != reported {
                shell.log.log(&format!(
                    "forwarded {} job events to the windows in the last {} s ({} running, {})",
                    n - reported,
                    last_report.elapsed().as_secs(),
                    running.len(),
                    util::speed_short(total)
                ));
                reported = n;
            }
            last_report = Instant::now();
        }
    }
}

/// SIGTERM / SIGINT (logout, `kill`, Ctrl-C in a terminal) quit like the menu does: running
/// downloads are paused with their state saved and `caffeinate` is stopped.
async fn quit_on_signal(app: AppHandle, shell: Arc<Shell>) {
    use tokio::signal::unix::{signal, SignalKind};
    let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else {
        return;
    };
    let name = tokio::select! {
        _ = term.recv() => "SIGTERM",
        _ = int.recv() => "SIGINT",
    };
    shell.log.log(&format!("{name}: quitting"));
    shell.daemon.shutdown().await;
    app.exit(0);
}

/// While the island is collapsed, only its pill takes the mouse: poll the pointer and flip
/// `ignoresMouseEvents` as it enters or leaves the pill.
async fn island_mouse(app: AppHandle, shell: Arc<Shell>) {
    let mut iv = tokio::time::interval(Duration::from_millis(50));
    loop {
        iv.tick().await;
        let active = {
            let st = shell.island.lock().unwrap();
            st.shown && st.mode == IslandMode::Collapsed
        };
        if active {
            let app2 = app.clone();
            let _ = app.run_on_main_thread(move || windows::island_hit_test(&app2));
        }
    }
}
