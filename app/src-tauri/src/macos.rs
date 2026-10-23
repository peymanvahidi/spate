//! AppKit glue the Tauri API doesn't cover: non-activating panels, window levels, frames in
//! screen points, the notch geometry and the mouse location. Every function here must run on
//! the main thread (callers go through `AppHandle::run_on_main_thread`).

use std::collections::BTreeMap;
use std::sync::Mutex;

use objc2::runtime::{AnyClass, AnyObject};
use objc2::{define_class, ClassType, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSApplication, NSEvent, NSPanel, NSResponder, NSScreen, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSPoint, NSRect, NSSize, NSString};
use tauri::{Runtime, WebviewWindow};

define_class!(
    // SAFETY: NSPanel has no subclassing requirements; no Drop, no ivars. Tao's window object
    // is re-classed to this (it keeps its own, larger allocation), the same trick tauri-nspanel
    // uses: a borderless NSPanel can be key without activating the app.
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpatePanel"]
    struct SpatePanel;

    impl SpatePanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            false
        }
    }
);

/// `NSStatusWindowLevel`: the menu bar's own layer, above full-screen apps' content.
pub const LEVEL_STATUS: isize = 25;
/// `NSFloatingWindowLevel`.
pub const LEVEL_FLOATING: isize = 3;

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("AppKit call off the main thread")
}

/// The window's NSWindow. Borrowed from Tauri, which keeps it alive while the window exists.
fn ns_window<R: Runtime>(w: &WebviewWindow<R>) -> Option<&NSWindow> {
    let p = w.ns_window().ok()? as *const NSWindow;
    // SAFETY: Tauri hands out its live NSWindow pointer; we only use it on the main thread
    // while the Tauri window (and so the NSWindow) exists.
    unsafe { p.as_ref() }
}

/// Turn a borderless Tauri window into a non-activating panel at `level` that shows on every
/// Space, including over full-screen apps. Clicking it never activates Spate.
pub fn make_panel<R: Runtime>(w: &WebviewWindow<R>, level: isize) {
    let Some(win) = ns_window(w) else { return };
    let cls: &AnyClass = SpatePanel::class();
    let obj: &AnyObject = win.as_ref();
    if obj.class() != cls {
        // Remember the real class (WebKit's KVO has already swapped in an
        // `NSKVONotifying_TaoWindow`): it must be put back before the window is torn down, or
        // removing WebKit's observers throws and aborts the process. See `unmake_panel`.
        ORIGINAL_CLASS.lock().unwrap().insert(obj as *const AnyObject as usize, obj.class() as *const AnyClass as usize);
        // SAFETY: SpatePanel descends from NSWindow through NSPanel and adds no ivars, so the
        // existing (TaoWindow-sized) allocation stays valid; see the class comment.
        unsafe { AnyObject::set_class(obj, cls) };
    }
    win.setStyleMask(win.styleMask() | NSWindowStyleMask::NonactivatingPanel);
    // SAFETY: re-classed to an NSPanel just above.
    let panel: &NSPanel = unsafe { &*(win as *const NSWindow as *const NSPanel) };
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    win.setHidesOnDeactivate(false);
    win.setLevel(level);
    win.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
}

/// Window → its class before `make_panel` (pointers as integers).
static ORIGINAL_CLASS: Mutex<BTreeMap<usize, usize>> = Mutex::new(BTreeMap::new());

/// Undo `make_panel` (class and style). Call it before the window is closed for good.
pub fn unmake_panel<R: Runtime>(w: &WebviewWindow<R>) {
    let Some(win) = ns_window(w) else { return };
    let obj: &AnyObject = win.as_ref();
    let Some(orig) = ORIGINAL_CLASS.lock().unwrap().remove(&(obj as *const AnyObject as usize)) else { return };
    win.orderOut(None);
    win.setStyleMask(win.styleMask() - NSWindowStyleMask::NonactivatingPanel);
    // SAFETY: `orig` is the class this very object had before `make_panel` (classes live for
    // the whole process), so it fits the allocation.
    unsafe { AnyObject::set_class(obj, &*(orig as *const AnyClass)) };
}

/// Show without activating the app or taking keyboard focus.
pub fn order_front<R: Runtime>(w: &WebviewWindow<R>) {
    if let Some(win) = ns_window(w) {
        win.orderFrontRegardless();
    }
}

pub fn set_ignores_mouse<R: Runtime>(w: &WebviewWindow<R>, ignore: bool) {
    if let Some(win) = ns_window(w) {
        win.setIgnoresMouseEvents(ignore);
    }
}

/// Place the window at a frame in screen points. Unlike Tauri's positioning, this doesn't keep
/// the window out of the menu bar.
pub fn set_frame<R: Runtime>(w: &WebviewWindow<R>, r: Rect) {
    if let Some(win) = ns_window(w) {
        win.setFrame_display(r.into(), true);
    }
}

pub fn activate_app() {
    let app = NSApplication::sharedApplication(mtm());
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

/// A blocking critical alert (for errors before anything else is up).
pub fn alert(title: &str, text: &str) {
    let a = NSAlert::new(mtm());
    a.setMessageText(&NSString::from_str(title));
    a.setInformativeText(&NSString::from_str(text));
    a.setAlertStyle(NSAlertStyle::Critical);
    a.runModal();
}

/// Mouse position in global screen points.
pub fn mouse() -> (f64, f64) {
    let p: NSPoint = NSEvent::mouseLocation();
    (p.x, p.y)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn contains(&self, (px, py): (f64, f64)) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn max_x(&self) -> f64 {
        self.x + self.w
    }
    pub fn max_y(&self) -> f64 {
        self.y + self.h
    }
}

impl From<NSRect> for Rect {
    fn from(r: NSRect) -> Self {
        Rect { x: r.origin.x, y: r.origin.y, w: r.size.width, h: r.size.height }
    }
}

impl From<Rect> for NSRect {
    fn from(r: Rect) -> Self {
        NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.w, r.h))
    }
}

/// A display: full frame and the part below the menu bar / above the Dock.
#[derive(Debug, Clone, Copy)]
pub struct Screen {
    pub frame: Rect,
    pub visible: Rect,
    /// Notch width and menu-bar height in points, for a display with a camera housing.
    pub notch: Option<(f64, f64)>,
}

fn describe(s: &NSScreen) -> Screen {
    let frame = Rect::from(s.frame());
    let top = s.safeAreaInsets().top;
    let notch = if top > 0.0 {
        let l = s.auxiliaryTopLeftArea();
        let r = s.auxiliaryTopRightArea();
        let w = frame.w - l.size.width - r.size.width;
        (w > 0.0 && l.size.width > 0.0 && r.size.width > 0.0).then_some((w, top))
    } else {
        None
    };
    Screen { frame, visible: Rect::from(s.visibleFrame()), notch }
}

fn screens() -> Vec<Screen> {
    NSScreen::screens(mtm()).iter().map(|s| describe(&s)).collect()
}

/// Where the island lives: the display with a notch (the built-in one), else the primary
/// display (the one with the menu bar at the origin).
pub fn island_screen() -> Option<Screen> {
    let all = screens();
    all.iter().find(|s| s.notch.is_some()).or_else(|| all.first()).copied()
}

/// The display under the mouse (falls back to the primary one).
pub fn screen_with_mouse() -> Option<Screen> {
    let m = mouse();
    let all = screens();
    all.iter().find(|s| s.frame.contains(m)).or_else(|| all.first()).copied()
}

