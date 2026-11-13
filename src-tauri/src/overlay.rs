//! The microphone bubble: a small always-on-top window that never takes focus and lets clicks
//! pass through, so keystrokes keep going to the app the user is dictating into.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, IsWindowVisible, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

const LABEL: &str = "bubble";

/// Whether the dot shows a faint trace of the words being heard.
pub static SHOW_WORDS: AtomicBool = AtomicBool::new(true);
static APP: OnceLock<AppHandle> = OnceLock::new();

/// Sends the words being heard right now (empty clears the dot).
pub fn words(text: &str) {
    if let Some(app) = APP.get() {
        let _ = app.emit_to(LABEL, "words", if SHOW_WORDS.load(Ordering::SeqCst) { text } else { "" });
    }
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let _ = APP.set(app.clone());
    let w = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("bubble.html".into()))
        .title("Better Dictation")
        .inner_size(140.0, 140.0)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .visible(false)
        .on_page_load(|_, p| crate::logger::trace(&format!("bubble page {:?}", p.event())))
        .build()?;
    w.set_ignore_cursor_events(true)?;
    let hwnd = hwnd(&w)?;
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize);
    }
    Ok(())
}

fn hwnd(w: &WebviewWindow) -> tauri::Result<HWND> {
    Ok(HWND(w.hwnd()?.0 as _))
}

/// Tints the bubble when dictation is running in an app other than the one the user is in.
pub fn away(app: &AppHandle, away: bool) {
    let _ = app.emit_to(LABEL, "away", away);
}

pub fn show(app: &AppHandle) {
    set_visible(app, true);
}

pub fn hide(app: &AppHandle) {
    set_visible(app, false);
}

/// Bumped on every show/hide so a delayed hide never closes a bubble that was shown again meanwhile.
static SHOWN: AtomicU32 = AtomicU32::new(0);
/// How long the bubble takes to shrink away before its window is hidden.
const SHRINK_MS: u64 = 650;

fn set_visible(app: &AppHandle, visible: bool) {
    let _ = app.emit_to(LABEL, "listening", visible);
    let token = SHOWN.fetch_add(1, Ordering::SeqCst) + 1;
    if !visible {
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(SHRINK_MS));
            if SHOWN.load(Ordering::SeqCst) == token {
                place(&app, false);
            }
        });
        return;
    }
    place(app, true);
}

fn place(app: &AppHandle, visible: bool) {
    let handle = app.clone();
    // Window calls belong on the thread that owns the window.
    let _ = app.run_on_main_thread(move || {
        let Some(w) = handle.get_webview_window(LABEL) else { return };
        let Ok(hwnd) = hwnd(&w) else { return };
        unsafe {
            if !visible {
                let _ = ShowWindow(hwnd, SW_HIDE);
                return;
            }
            // Bottom-centre of the monitor the user is working on.
            let mon = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let mut rect = RECT::default();
            if GetMonitorInfoW(mon, &mut info).as_bool() && GetWindowRect(hwnd, &mut rect).is_ok() {
                let work = info.rcWork;
                let (ww, wh) = (rect.right - rect.left, rect.bottom - rect.top);
                let margin = (32.0 * w.scale_factor().unwrap_or(1.0)) as i32;
                let x = work.left + (work.right - work.left - ww) / 2;
                let y = work.bottom - wh - margin;
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
                let sc = w.scale_factor().unwrap_or(1.0);
                crate::hook::DOT[0].store(x + ww / 2, Ordering::SeqCst);
                crate::hook::DOT[1].store(y + wh / 2, Ordering::SeqCst);
                crate::hook::DOT[2].store((16.0 * sc) as i32, Ordering::SeqCst);
            }
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let mut at = RECT::default();
            let _ = GetWindowRect(hwnd, &mut at);
            crate::logger::trace(&format!(
                "bubble shown: visible {}, at {},{} size {}x{}",
                IsWindowVisible(hwnd).as_bool(),
                at.left,
                at.top,
                at.right - at.left,
                at.bottom - at.top
            ));
        }
    });
}
