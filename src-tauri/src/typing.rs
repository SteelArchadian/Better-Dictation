//! Types text into whatever window has focus, as Unicode key events.
//!
//! SendInput marks these as injected, so our own keyboard hook ignores them and they never count
//! as "the user pressed a key".

use std::time::{Duration, Instant};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    SendInput, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN,
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GUITHREADINFO, GetAncestor, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, IsWindow,
    PostMessageW, SendMessageW, WM_ACTIVATE, WM_CHAR, WM_KEYDOWN, WM_KEYUP,
};

pub fn foreground() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

/// The control that has keyboard focus in the foreground window (what a keypress would reach).
pub fn focus_target() -> isize {
    unsafe {
        let mut info = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        if GetGUIThreadInfo(0, &mut info).is_ok() && !info.hwndFocus.is_invalid() {
            return info.hwndFocus.0 as isize;
        }
        GetForegroundWindow().0 as isize
    }
}

/// Chromium browsers (Opera GX, Chrome, Edge) take characters sent straight to their input
/// window as one quick insert instead of replaying key presses, and the clipboard stays untouched.
fn chromium_input() -> Option<isize> {
    let hwnd = HWND(focus_target() as *mut _);
    let mut name = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut name) } as usize;
    String::from_utf16_lossy(&name[..n]).starts_with("Chrome_RenderWidgetHostHWND").then_some(hwnd.0 as isize)
}

/// Whether `target` still exists (the user may have closed the app they were dictating into).
pub fn alive(target: isize) -> bool {
    unsafe { IsWindow(Some(HWND(target as *mut _))).as_bool() }
}

/// Chromium drops or reorders posted messages that arrive faster than about one per 20 ms
/// (measured against a Chrome textarea), which showed up as doubled and missing letters.
const POST_GAP: Duration = Duration::from_millis(20);

fn post(hwnd: isize, msg: u32, w: usize, l: isize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(w), LPARAM(l));
    }
    std::thread::sleep(POST_GAP);
}

/// An unfocused Chromium window ignores posted characters until its top-level window has been told
/// it is active, so tell it that (without actually focusing it) when another app is in front.
fn wake(target: isize) {
    unsafe {
        let root = GetAncestor(HWND(target as *mut _), GA_ROOT);
        if !root.is_invalid() && root != GetForegroundWindow() {
            SendMessageW(root, WM_ACTIVATE, Some(WPARAM(1)), Some(LPARAM(0)));
        }
    }
}

fn post_backspaces(hwnd: isize, n: usize) {
    for _ in 0..n {
        post(hwnd, WM_KEYDOWN, VK_BACK.0 as usize, 0x000E_0001);
        post(hwnd, WM_KEYUP, VK_BACK.0 as usize, 0xC00E_0001u32 as isize);
    }
}

fn post_chars(hwnd: isize, text: &str) {
    for u in text.encode_utf16() {
        post(hwnd, WM_CHAR, u as usize, 1);
    }
}

/// Like `replace`, but sent to one control by window messages, so it works while another app has
/// focus (and never touches the keyboard state or the clipboard).
pub fn replace_in(target: isize, old: &str, new: &str) {
    wake(target);
    let same = old.chars().zip(new.chars()).take_while(|(a, b)| a == b).count();
    post_backspaces(target, old.chars().count() - same);
    post_chars(target, &new.chars().skip(same).collect::<String>());
}

pub fn type_in(target: isize, text: &str) {
    wake(target);
    post_chars(target, text);
}

/// A still-held Win, Ctrl or Alt would turn typed letters into shortcuts (Win+H is often still
/// down when the first word is ready), so wait for them to be released first.
fn wait_for_modifiers() {
    let held = |k: VIRTUAL_KEY| unsafe { GetAsyncKeyState(k.0 as i32) } < 0;
    let deadline = Instant::now() + Duration::from_secs(3);
    while [VK_LWIN, VK_RWIN, VK_CONTROL, VK_MENU].into_iter().any(held) {
        if Instant::now() > deadline {
            crate::logger::log("modifier still held after 3 s; typing anyway");
            return;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Turns `old` (just typed, caret right after it) into `new`: backspaces over the part that
/// differs and types the rest, so the shared beginning never flickers.
pub fn replace(old: &str, new: &str) {
    let same = old.chars().zip(new.chars()).take_while(|(a, b)| a == b).count();
    let erase = old.chars().count() - same;
    if let Some(hwnd) = chromium_input() {
        replace_in(hwnd, old, new);
        return;
    }
    if erase > 0 {
        wait_for_modifiers();
        let key = |up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_BACK,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                    ..Default::default()
                },
            },
        };
        let inputs: Vec<INPUT> = (0..erase).flat_map(|_| [key(false), key(true)]).collect();
        unsafe {
            SendInput(&inputs, size_of::<INPUT>() as i32);
        }
    }
    type_text(&new.chars().skip(same).collect::<String>());
}

pub fn type_text(text: &str) {
    if text.is_empty() {
        return;
    }
    wait_for_modifiers();
    if let Some(hwnd) = chromium_input() {
        post_chars(hwnd, text);
        return;
    }

    let key = |unit: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wScan: unit,
                dwFlags: if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE },
                ..Default::default()
            },
        },
    };
    let inputs: Vec<INPUT> = text.encode_utf16().flat_map(|u| [key(u, false), key(u, true)]).collect();
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        crate::logger::log(&format!("typing blocked after {sent}/{} events (target may be elevated)", inputs.len()));
    }
}
