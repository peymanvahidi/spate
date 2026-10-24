//! The Tauri commands behind `app/ui/js/api.js`. Errors are the daemon's `RpcError`
//! (`{code, message}`), so the UI sees the same codes as over the socket.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use serde_json::{json, Value};
use spate_proto::{AddRequest, JobState, JobView, RpcError};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

use crate::windows::{self, IslandMode};
use crate::Shell;

type R<T> = Result<T, RpcError>;

fn err(code: &str, message: impl Into<String>) -> RpcError {
    RpcError { code: code.into(), message: message.into() }
}

#[tauri::command]
pub async fn list(shell: State<'_, Arc<Shell>>) -> R<Vec<JobView>> {
    Ok(shell.daemon.list())
}

#[tauri::command]
pub async fn add(shell: State<'_, Arc<Shell>>, request: AddRequest) -> R<Value> {
    let id = shell.daemon.add(request).await?;
    Ok(json!({ "job_id": id }))
}

#[tauri::command]
pub async fn pause(shell: State<'_, Arc<Shell>>, job_id: String) -> R<Value> {
    shell.daemon.pause(&job_id).await?;
    Ok(json!({}))
}

#[tauri::command]
pub async fn resume(shell: State<'_, Arc<Shell>>, job_id: String) -> R<Value> {
    shell.daemon.resume(&job_id).await?;
    Ok(json!({}))
}

#[tauri::command]
pub async fn cancel(shell: State<'_, Arc<Shell>>, job_id: String) -> R<Value> {
    shell.daemon.cancel(&job_id).await?;
    shell.dest.lock().unwrap().remove(&job_id);
    Ok(json!({}))
}

#[tauri::command]
pub async fn remove(shell: State<'_, Arc<Shell>>, job_id: String) -> R<Value> {
    shell.daemon.remove(&job_id).await?;
    Ok(json!({}))
}

/// Where a job's bytes are on disk right now: the finished file, else its `.spate-part`.
fn on_disk(shell: &Shell, job: &JobView) -> Option<PathBuf> {
    if let Some(p) = job.path.as_ref().filter(|p| p.exists()) {
        return Some(p.clone());
    }
    let dir = &shell.daemon.settings().download_dir;
    let part = dir.join(format!("{}{}", job.filename, spate_proto::PART_SUFFIX));
    part.exists().then_some(part)
}

pub fn reveal_job(shell: &Shell, job_id: &str) -> R<()> {
    let job = shell.job(job_id).ok_or_else(|| err("not_found", format!("no job {job_id}")))?;
    match on_disk(shell, &job) {
        Some(p) => run_open(&["-R".as_ref(), p.as_os_str()]),
        None => {
            // Nothing on disk (cancelled, or not started): show the downloads folder.
            let dir = shell.daemon.settings().download_dir.clone();
            run_open(&[dir.as_os_str()])
        }
    }
}

fn run_open(args: &[&std::ffi::OsStr]) -> R<()> {
    let st = Command::new("/usr/bin/open").args(args).status().map_err(|e| err("internal", e.to_string()))?;
    if st.success() {
        Ok(())
    } else {
        Err(err("internal", format!("open exited with {st}")))
    }
}

/// Show the file (or its `.spate-part`) in Finder.
#[tauri::command]
pub async fn reveal(shell: State<'_, Arc<Shell>>, job_id: String) -> R<Value> {
    reveal_job(&shell, &job_id)?;
    Ok(json!({}))
}

/// Open the finished file with its default app.
#[tauri::command]
pub async fn open(shell: State<'_, Arc<Shell>>, job_id: String) -> R<Value> {
    let job = shell.job(&job_id).ok_or_else(|| err("not_found", format!("no job {job_id}")))?;
    if job.state != JobState::Done {
        return Err(err("bad_state", "the download hasn't finished"));
    }
    let path = job.path.filter(|p| p.exists()).ok_or_else(|| err("not_found", "the file is no longer there"))?;
    run_open(&[path.as_os_str()])?;
    Ok(json!({}))
}

/// `main` → the main window. `settings` → settings.toml in TextEdit (no settings screen yet).
#[tauri::command]
pub async fn open_window(app: AppHandle, shell: State<'_, Arc<Shell>>, name: String) -> R<Value> {
    match name.as_str() {
        "main" => windows::open_main(&app),
        "settings" => {
            if let Some(p) = app.get_webview_window(windows::POPOVER) {
                let _ = p.hide();
            }
            let file = shell.data_dir.join("settings.toml");
            if !file.exists() {
                let _ = std::fs::write(&file, "");
            }
            run_open(&["-e".as_ref(), file.as_os_str()])?;
        }
        other => return Err(err("bad_request", format!("unknown window {other}"))),
    }
    Ok(json!({}))
}

/// Native folder picker for the capture card. The file goes there when the job finishes (at
/// once if it already has). Returns the folder, or null if the picker was cancelled.
#[tauri::command]
pub async fn choose_folder(app: AppHandle, shell: State<'_, Arc<Shell>>, job_id: String) -> R<Option<String>> {
    let job = shell.job(&job_id).ok_or_else(|| err("not_found", format!("no job {job_id}")))?;
    let start = shell.dest.lock().unwrap().get(&job_id).cloned().unwrap_or_else(|| shell.daemon.settings().download_dir.clone());
    let _ = app.run_on_main_thread(crate::macos::activate_app);
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Save the download in…")
        .set_directory(&start)
        .pick_folder(move |p| {
            let _ = tx.send(p.and_then(|p| p.into_path().ok()));
        });
    let Some(dir) = rx.await.ok().flatten() else { return Ok(None) };
    let shown = dir.display().to_string();
    if job.state == JobState::Done {
        let v = crate::move_finished(&shell, job, dir).await;
        let _ = app.emit("job", &v);
    } else if job.state == JobState::Failed {
        return Err(err("bad_state", "the download failed"));
    } else {
        shell.log.log(&format!("job {job_id}: will move to a chosen folder when done"));
        shell.dest.lock().unwrap().insert(job_id, dir);
    }
    Ok(Some(shown))
}

/// The island page's state: hidden (nothing to show), collapsed (only the pill takes the
/// mouse) or expanded (the whole window is interactive).
#[tauri::command]
pub async fn set_island(app: AppHandle, shell: State<'_, Arc<Shell>>, mode: String) -> R<Value> {
    let mode = match mode.as_str() {
        "hidden" => IslandMode::Hidden,
        "collapsed" => IslandMode::Collapsed,
        "expanded" => IslandMode::Expanded,
        other => return Err(err("bad_request", format!("unknown island mode {other}"))),
    };
    let running = shell.daemon.list().iter().any(|j| j.state == JobState::Running);
    {
        let mut st = shell.island.lock().unwrap();
        st.mode = mode;
        st.ignoring = None;
    }
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(w) = app2.get_webview_window(windows::ISLAND) else { return };
        match mode {
            IslandMode::Hidden if !running => windows::hide_island(&app2, &w),
            IslandMode::Hidden => crate::macos::set_ignores_mouse(&w, true),
            IslandMode::Expanded => crate::macos::set_ignores_mouse(&w, false),
            IslandMode::Collapsed => windows::island_hit_test(&app2),
        }
    });
    Ok(json!({}))
}
