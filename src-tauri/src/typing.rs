//! Types text into whatever window has focus, as Unicode key events.
//!
//! SendInput marks these as injected, so our own keyboard hook ignores them and they never count
//! as "the user pressed a key".

use std::time::{Duration, Instant};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    SendInput, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

pub fn foreground() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
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
