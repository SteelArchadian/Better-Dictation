//! The microphone bubble: a small always-on-top window that never takes focus and lets clicks
//! pass through, so keystrokes keep going to the app the user is dictating into.

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

const LABEL: &str = "bubble";

pub fn create(app: &AppHandle) -> tauri::Result<()> {
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

pub fn show(app: &AppHandle) {
    set_visible(app, true);
}

pub fn hide(app: &AppHandle) {
    set_visible(app, false);
}

fn set_visible(app: &AppHandle, visible: bool) {
    let _ = app.emit_to(LABEL, "listening", visible);
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
            }
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    });
}
