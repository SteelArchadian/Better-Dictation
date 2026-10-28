//! Low-level keyboard and mouse hooks.
//!
//! RegisterHotKey can't claim Win+H (Windows reserves it), so a WH_KEYBOARD_LL hook watches for
//! the shortcut and swallows it. While listening, a WH_MOUSE_LL hook is installed so any click
//! stops dictation; it is removed again when listening ends so it costs nothing while idle.
//! Events we inject ourselves (typed text, the Start-menu suppressor) are ignored.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
    VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT,
    LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW,
    PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_RBUTTONDOWN,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
};

use crate::Event;
use crate::settings::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_WIN, Shortcut};

/// The shortcut is active (when off, the key passes through to Windows untouched).
pub static ENABLED: AtomicBool = AtomicBool::new(true);
/// Dictation is running: real key presses and clicks stop it.
pub static LISTENING: AtomicBool = AtomicBool::new(false);
/// The settings window is waiting for a new shortcut; the next combination is captured.
pub static CAPTURING: AtomicBool = AtomicBool::new(false);

static SHORTCUT: AtomicU32 = AtomicU32::new(0);
/// The shortcut's key is held down; its auto-repeats are swallowed instead of toggling again.
static TRIGGER_HELD: AtomicBool = AtomicBool::new(false);
static CAPTURE_HELD: AtomicU32 = AtomicU32::new(0);
static SENDER: OnceLock<Sender<Event>> = OnceLock::new();
static THREAD_ID: AtomicU32 = AtomicU32::new(0);

const MSG_MOUSE_ON: u32 = WM_APP + 1;
const MSG_MOUSE_OFF: u32 = WM_APP + 2;
/// An unassigned virtual key. Tapping it after a swallowed Win+key keeps the lone Win release
/// from opening the Start menu (the same trick PowerToys uses).
const VK_DUMMY: u16 = 0xE8;

pub fn set_shortcut(s: Shortcut) {
    SHORTCUT.store(s.pack(), Ordering::SeqCst);
}

pub fn start(tx: Sender<Event>) {
    let _ = SENDER.set(tx);
    std::thread::Builder::new()
        .name("input-hook".into())
        .spawn(|| unsafe { hook_thread() })
        .expect("spawn hook thread");
}

/// Install or remove the mouse hook. Safe to call from any thread.
pub fn watch_mouse(on: bool) {
    let id = THREAD_ID.load(Ordering::SeqCst);
    if id != 0 {
        let msg = if on { MSG_MOUSE_ON } else { MSG_MOUSE_OFF };
        unsafe {
            let _ = PostThreadMessageW(id, msg, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe fn hook_thread() {
    unsafe {
        // Make sure this thread has a message queue before anyone posts to it.
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let hmod = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
        match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), hmod, 0) {
            Ok(_) => crate::logger::log("keyboard hook installed"),
            Err(e) => {
                crate::logger::log(&format!("keyboard hook FAILED: {e}"));
                return;
            }
        }
        THREAD_ID.store(GetCurrentThreadId(), Ordering::SeqCst);

        let mut mouse: Option<HHOOK> = None;
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                MSG_MOUSE_ON if mouse.is_none() => {
                    mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hmod, 0).ok();
                }
                MSG_MOUSE_OFF => {
                    if let Some(h) = mouse.take() {
                        let _ = UnhookWindowsHookEx(h);
                    }
                }
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

fn send(e: Event) {
    if let Some(tx) = SENDER.get() {
        let _ = tx.send(e);
    }
}

fn is_modifier(vk: u32) -> bool {
    [
        VK_LWIN, VK_RWIN, VK_CONTROL, VK_LCONTROL, VK_RCONTROL, VK_MENU, VK_LMENU, VK_RMENU,
        VK_SHIFT, VK_LSHIFT, VK_RSHIFT,
    ]
    .iter()
    .any(|k| k.0 as u32 == vk)
}

fn current_mods() -> u32 {
    let down = |k: VIRTUAL_KEY| unsafe { GetAsyncKeyState(k.0 as i32) } < 0;
    let mut m = 0;
    if down(VK_LWIN) || down(VK_RWIN) {
        m |= MOD_WIN;
    }
    if down(VK_CONTROL) {
        m |= MOD_CTRL;
    }
    if down(VK_MENU) {
        m |= MOD_ALT;
    }
    if down(VK_SHIFT) {
        m |= MOD_SHIFT;
    }
    m
}

fn tap_dummy_key() {
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(VK_DUMMY), dwFlags: flags, ..Default::default() },
        },
    };
    let inputs = [key(Default::default()), key(KEYEVENTF_KEYUP)];
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Returns true when the key event must be swallowed.
fn on_key(vk: u32, down: bool, up: bool) -> bool {
    if is_modifier(vk) {
        return false;
    }

    if CAPTURING.load(Ordering::SeqCst) {
        if down {
            let mods = current_mods();
            CAPTURING.store(false, Ordering::SeqCst);
            CAPTURE_HELD.store(vk, Ordering::SeqCst);
            if vk == VK_ESCAPE.0 as u32 && mods == 0 {
                send(Event::CaptureCancelled);
            } else {
                if mods & MOD_WIN != 0 {
                    tap_dummy_key();
                }
                send(Event::Captured(Shortcut { vk, mods }));
            }
            return true;
        }
    }
    if up && CAPTURE_HELD.load(Ordering::SeqCst) == vk {
        CAPTURE_HELD.store(0, Ordering::SeqCst);
        return true;
    }

    let sc = Shortcut::unpack(SHORTCUT.load(Ordering::SeqCst));
    if vk == sc.vk {
        if down {
            if TRIGGER_HELD.load(Ordering::SeqCst) {
                return true;
            }
            if ENABLED.load(Ordering::SeqCst) && current_mods() == sc.mods {
                TRIGGER_HELD.store(true, Ordering::SeqCst);
                if sc.mods & MOD_WIN != 0 {
                    tap_dummy_key();
                }
                send(Event::Toggle);
                return true;
            }
        } else if up && TRIGGER_HELD.swap(false, Ordering::SeqCst) {
            return true;
        }
    }

    if down && LISTENING.load(Ordering::SeqCst) {
        let esc = vk == VK_ESCAPE.0 as u32;
        send(Event::Stop(if esc { "esc".into() } else { format!("key {vk:#04x}") }));
        // Esc only means "stop"; any other key still reaches the app.
        return vk == VK_ESCAPE.0 as u32;
    }
    false
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if kb.flags.0 & LLKHF_INJECTED.0 == 0 {
                let m = wparam.0 as u32;
                let down = m == WM_KEYDOWN || m == WM_SYSKEYDOWN;
                let up = m == WM_KEYUP || m == WM_SYSKEYUP;
                if on_key(kb.vkCode, down, up) {
                    return LRESULT(1);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 && LISTENING.load(Ordering::SeqCst) {
            let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            let m = wparam.0 as u32;
            let click = matches!(m, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN);
            if click && ms.flags & LLMHF_INJECTED == 0 {
                send(Event::Stop("click".into()));
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}
