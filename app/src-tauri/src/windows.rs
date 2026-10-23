//! The four kinds of windows: main (regular window, hidden instead of closed), popover (under the
//! menu-bar icon, hidden on blur), capture cards (one per Brave job, top-right under the menu
//! bar) and the notch island (click-through outside the visible pill).

use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::utils::config::WindowEffectsConfig;
use tauri::window::{Effect, EffectState};
use tauri::{
    ActivationPolicy, AppHandle, LogicalPosition, Manager, TitleBarStyle, Url, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};

use crate::macos::{self, Rect};
use crate::Shell;

pub const MAIN: &str = "main";
pub const POPOVER: &str = "popover";
pub const ISLAND: &str = "island";

const POPOVER_W: f64 = 360.0;
const POPOVER_H: f64 = 480.0;
const CARD_W: f64 = 380.0;
const CARD_H: f64 = 150.0;
/// Big enough for the expanded island (420 × ≤184, plus the ears and its shadow), also in the
/// no-notch layout where the pill floats 30 pt below the top.
const ISLAND_W: f64 = 460.0;
const ISLAND_H: f64 = 250.0;

fn effects(effect: Effect, radius: f64) -> WindowEffectsConfig {
    WindowEffectsConfig { effects: vec![effect], state: Some(EffectState::Active), radius: Some(radius), ..Default::default() }
}

/// Create the long-lived windows, all hidden. They're hidden rather than closed later, so
/// their pages (and speed-graph history) stay alive.
pub fn create_all(app: &AppHandle) -> tauri::Result<()> {
    let main = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::App("main.html".into()))
        .title("Spate")
        .inner_size(1040.0, 680.0)
        .min_inner_size(760.0, 460.0)
        .title_bar_style(TitleBarStyle::Overlay)
        .hidden_title(true)
        .traffic_light_position(LogicalPosition::new(18.0, 22.0))
        .transparent(true)
        .effects(effects(Effect::Sidebar, 0.0))
        .visible(false)
        .build()?;
    main.on_window_event({
        let app = app.clone();
        move |ev| {
            if let WindowEvent::CloseRequested { api, .. } = ev {
                api.prevent_close();
                hide_main(&app);
            }
        }
    });

    let pop = WebviewWindowBuilder::new(app, POPOVER, WebviewUrl::App("popover.html".into()))
        .title("Spate")
        .inner_size(POPOVER_W, POPOVER_H)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .shadow(true)
        .accept_first_mouse(true)
        .effects(effects(Effect::Popover, 12.0))
        .visible(false)
        .build()?;
    pop.on_window_event({
        let app = app.clone();
        move |ev| match ev {
            WindowEvent::Focused(false) => hide_popover(&app),
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                hide_popover(&app);
            }
            _ => {}
        }
    });

    // setup() runs on the main thread, so the display can be measured right here.
    let query = island_geometry().map(|g| g.query).unwrap_or_else(|| "nonotch=1".into());
    app.state::<Arc<Shell>>().island.lock().unwrap().query = query.clone();
    crate::log(app, &format!("island: {query}"));
    let island = WebviewWindowBuilder::new(app, ISLAND, WebviewUrl::App(format!("island.html?{query}").into()))
        .title("Spate island")
        .inner_size(ISLAND_W, ISLAND_H)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .shadow(false)
        .skip_taskbar(true)
        .focused(false)
        .accept_first_mouse(true)
        .visible(false)
        .build()?;
    island.on_window_event(|ev| {
        if let WindowEvent::CloseRequested { api, .. } = ev {
            api.prevent_close();
        }
    });
    let w = island.clone();
    app.run_on_main_thread(move || {
        macos::make_panel(&w, macos::LEVEL_STATUS);
        macos::set_ignores_mouse(&w, true);
    })?;
    Ok(())
}

pub fn open_main(app: &AppHandle) {
    if let Some(p) = app.get_webview_window(POPOVER) {
        let _ = p.hide();
    }
    let _ = app.set_activation_policy(ActivationPolicy::Regular);
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    let _ = app.run_on_main_thread(macos::activate_app);
}

fn hide_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.hide();
    }
    let _ = app.set_activation_policy(ActivationPolicy::Accessory);
}

fn hide_popover(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(POPOVER) {
        if w.is_visible().unwrap_or(false) {
            *app.state::<Arc<Shell>>().popover_hidden_at.lock().unwrap() = Some(Instant::now());
            let _ = w.hide();
        }
    }
}

/// Left click on the menu-bar icon: show the popover under it, or hide it.
pub fn toggle_popover(app: &AppHandle) {
    let Some(w) = app.get_webview_window(POPOVER) else { return };
    if w.is_visible().unwrap_or(false) {
        hide_popover(app);
        return;
    }
    // The click that blurred (and so hid) the popover must not reopen it right away.
    let shell = app.state::<Arc<Shell>>();
    if shell.popover_hidden_at.lock().unwrap().is_some_and(|t| t.elapsed() < Duration::from_millis(300)) {
        return;
    }
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(w) = app2.get_webview_window(POPOVER) else { return };
        if let Some(s) = macos::screen_with_mouse() {
            // Centred under the click (the icon), kept on screen, just below the menu bar.
            let (mx, _) = macos::mouse();
            let vf = s.visible;
            let x = (mx - POPOVER_W / 2.0).clamp(vf.x + 6.0, vf.max_x() - POPOVER_W - 6.0);
            let y = vf.max_y() - 5.0 - POPOVER_H;
            macos::set_frame(&w, Rect { x, y, w: POPOVER_W, h: POPOVER_H });
        }
        let _ = w.show();
        let _ = w.set_focus();
        macos::activate_app();
    });
}

/// The capture card for a job Brave handed over: borderless, always on top, top-right under
/// the menu bar on the screen with the mouse. Cards stack downwards. Shown without taking focus.
pub fn show_card(app: &AppHandle, job_id: &str) {
    let short: String = job_id.chars().filter(|c| c.is_ascii_alphanumeric()).take(12).collect();
    let label = format!("card-{short}");
    if app.get_webview_window(&label).is_some() {
        return;
    }
    let stack = app.webview_windows().keys().filter(|l| l.starts_with("card-")).count();
    let url = WebviewUrl::App(format!("card.html?job={job_id}").into());
    let built = WebviewWindowBuilder::new(app, &label, url)
        .title("Spate — New download")
        .inner_size(CARD_W, CARD_H)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .skip_taskbar(true)
        .shadow(true)
        .focused(false)
        .accept_first_mouse(true)
        .effects(effects(Effect::HudWindow, 14.0))
        .visible(false)
        .build();
    let w = match built {
        Ok(w) => w,
        Err(e) => {
            crate::log(app, &format!("card window failed: {e}"));
            return;
        }
    };
    w.on_window_event({
        let (app, label) = (app.clone(), label.clone());
        move |ev| {
            if let WindowEvent::CloseRequested { .. } = ev {
                // Runs on the main thread just before the window is destroyed.
                if let Some(w) = app.get_webview_window(&label) {
                    macos::unmake_panel(&w);
                }
            }
        }
    });
    let _ = app.run_on_main_thread(move || {
        macos::make_panel(&w, macos::LEVEL_FLOATING);
        if let Some(s) = macos::screen_with_mouse() {
            let vf = s.visible;
            let y = vf.max_y() - 10.0 - CARD_H - stack as f64 * (CARD_H + 10.0);
            macos::set_frame(&w, Rect { x: vf.max_x() - CARD_W - 10.0, y: y.max(vf.y), w: CARD_W, h: CARD_H });
        }
        macos::order_front(&w);
    });
}

/// What the island page asked for (`set_island`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IslandMode {
    Hidden,
    Collapsed,
    Expanded,
}

/// The island's geometry on the current display.
#[derive(Debug, Clone)]
pub struct IslandGeom {
    /// Window frame (screen points).
    pub frame: Rect,
    /// The collapsed pill (screen points): the only part that takes the mouse when collapsed.
    pub pill: Rect,
    /// `notch=<w>&bar=<h>` or `nonotch=1`.
    pub query: String,
}

/// Main thread only.
pub fn island_geometry() -> Option<IslandGeom> {
    let s = macos::island_screen()?;
    let f = s.frame;
    let frame = Rect { x: f.x + (f.w - ISLAND_W) / 2.0, y: f.max_y() - ISLAND_H, w: ISLAND_W, h: ISLAND_H };
    // Pill in window coordinates from the top-left, mirroring island.css.
    let (pw, ph, top, query) = match s.notch {
        Some((nw, bar)) => (nw + 2.0 * 66.0, bar, 0.0, format!("notch={}&bar={}", nw.round(), bar.round())),
        None => (210.0, 30.0, 30.0, "nonotch=1".to_string()),
    };
    let pad = 2.0;
    let pill = Rect {
        x: frame.x + (ISLAND_W - pw) / 2.0 - pad,
        y: frame.max_y() - top - ph - pad,
        w: pw + 2.0 * pad,
        h: ph + 2.0 * pad,
    };
    Some(IslandGeom { frame, pill, query })
}

/// Show the island (main thread): re-measure the display, reload the page if the notch
/// geometry changed, place it and order it front without activating anything.
pub fn show_island(app: &AppHandle, w: &WebviewWindow) {
    let shell = app.state::<Arc<Shell>>();
    let Some(g) = island_geometry() else { return };
    {
        let mut st = shell.island.lock().unwrap();
        if st.query != g.query {
            st.query = g.query.clone();
            if let Ok(mut url) = w.url() {
                url.set_query(Some(&g.query));
                let _ = w.navigate(url);
            } else if let Ok(url) = Url::parse(&format!("tauri://localhost/island.html?{}", g.query)) {
                let _ = w.navigate(url);
            }
        }
        st.geom = Some(g.clone());
        st.shown = true;
        let ignore = st.mode != IslandMode::Expanded;
        st.ignoring = Some(ignore);
        macos::set_ignores_mouse(w, ignore);
    }
    macos::set_frame(w, g.frame);
    macos::order_front(w);
}

pub fn hide_island(app: &AppHandle, w: &WebviewWindow) {
    let shell = app.state::<Arc<Shell>>();
    shell.island.lock().unwrap().shown = false;
    let _ = w.hide();
}

/// Collapsed island: let the mouse through everywhere except over the pill. Called every
/// ~50 ms on the main thread while the island is up.
pub fn island_hit_test(app: &AppHandle) {
    let shell = app.state::<Arc<Shell>>();
    let mut st = shell.island.lock().unwrap();
    if !st.shown {
        return;
    }
    let Some(w) = app.get_webview_window(ISLAND) else { return };
    let want_ignore = match st.mode {
        IslandMode::Expanded => false,
        IslandMode::Hidden => true,
        IslandMode::Collapsed => !st.geom.as_ref().is_some_and(|g| g.pill.contains(macos::mouse())),
    };
    if st.ignoring != Some(want_ignore) {
        st.ignoring = Some(want_ignore);
        macos::set_ignores_mouse(&w, want_ignore);
    }
}

#[derive(Debug)]
pub struct IslandState {
    pub mode: IslandMode,
    pub shown: bool,
    pub query: String,
    pub geom: Option<IslandGeom>,
    pub ignoring: Option<bool>,
}

impl Default for IslandState {
    fn default() -> Self {
        // The page loads with no query; the first show sets it.
        IslandState { mode: IslandMode::Hidden, shown: false, query: String::new(), geom: None, ignoring: None }
    }
}
