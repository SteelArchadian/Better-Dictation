//! Better Dictation's text service (a Windows Text Services Framework input processor).
//!
//! Windows loads this DLL into every app that uses text input. It does nothing on its own: it connects
//! to the tray app's pipe and, when asked, inserts text at the caret of the focused text box through
//! TSF, so the app sees committed text, not key presses (no keydown events, no site hotkeys firing).
//!
//! Wire format, tray app to here: op u8 (1 = replace), erase i32 LE (UTF-16 units before the caret to
//! replace), count u32 LE, then `count` UTF-16 units LE. The reply is one byte: 1 done, 0 failed.
//! Nothing here may panic out into the host app, so every entry point is wrapped.

#![allow(non_snake_case)]

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_NOINTERFACE, E_POINTER, GENERIC_READ, GENERIC_WRITE,
    HANDLE, HWND, LPARAM, LRESULT, S_FALSE, S_OK, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::TextServices::{
    ITfContext, ITfEditSession, ITfEditSession_Impl, ITfRange, ITfTextInputProcessorEx,
    ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl, ITfThreadMgr, TF_ANCHOR_END, TF_DEFAULT_SELECTION,
    TF_ES_READWRITE, TF_ES_SYNC, TF_SELECTION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_MESSAGE, RegisterClassW, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WNDCLASSW,
};
use windows::core::{GUID, HRESULT, IUnknown, Interface, Ref, Result, implement, w};

/// Registered by the tray app; keep in sync with src-tauri/src/tip_reg.rs.
pub const CLSID_TIP: GUID = GUID::from_u128(0xB7E2C1A4_5D3F_4C8E_9A61_0D2F7B3E8C15);
const PIPE: &str = r"\\.\pipe\BetterDictationTip";
const WM_INSERT: u32 = WM_APP + 7;

struct Request {
    erase: i32,
    text: Vec<u16>,
}

thread_local! {
    /// The thread manager and client id of the TIP instance living on this thread.
    static STATE: RefCell<Option<(ITfThreadMgr, u32)>> = const { RefCell::new(None) };
}

#[implement(ITfEditSession)]
struct Edit {
    ctx: ITfContext,
    req: *const Request,
}

impl ITfEditSession_Impl for Edit_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        unsafe {
            let req = &*self.req;
            let mut sel = [TF_SELECTION::default()];
            let mut fetched = 0u32;
            self.ctx.GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched).inspect_err(|e| trace(&format!("GetSelection: {e}")))?;
            let range: ITfRange = ManuallyDrop::take(&mut sel[0].range).ok_or(E_FAIL)?;
            if req.erase > 0 {
                let mut shifted = 0i32;
                range.ShiftStart(ec, -req.erase, &mut shifted, std::ptr::null())?;
            }
            range.SetText(ec, 0, &req.text).inspect_err(|e| trace(&format!("SetText: {e}")))?;
            if std::env::var_os("BD_TIP_TRACE").is_some() {
                let mut buf = [0u16; 64];
                let mut got = 0u32;
                let _ = range.GetText(ec, 0, &mut buf, &mut got);
                trace(&format!("range now holds {:?}", String::from_utf16_lossy(&buf[..got as usize])));
            }
            range.Collapse(ec, TF_ANCHOR_END)?;
            sel[0].range = ManuallyDrop::new(Some(range));
            self.ctx.SetSelection(ec, &sel)?;
            Ok(())
        }
    }
}

/// Runs on the TIP's own (UI) thread: finds the focused text context and edits it.
fn trace(msg: &str) {
    if std::env::var_os("BD_TIP_TRACE").is_some() {
        eprintln!("bd_tip: {msg}");
    }
}

fn insert(req: &Request) -> bool {
    STATE
        .with(|s| {
            let s = s.borrow();
            let Some((tm, client)) = s.as_ref() else {
                trace("no thread manager");
                return None;
            };
            unsafe {
                let doc = tm.GetFocus().map_err(|e| trace(&format!("GetFocus: {e}"))).ok()?;
                let ctx = doc.GetTop().map_err(|e| trace(&format!("GetTop: {e}"))).ok()?;
                let session: ITfEditSession = Edit { ctx: ctx.clone(), req }.into();
                let hr = ctx
                    .RequestEditSession(*client, &session, TF_ES_SYNC | TF_ES_READWRITE)
                    .map_err(|e| trace(&format!("RequestEditSession: {e}")))
                    .ok()?;
                trace(&format!("edit session result {hr:?}"));
                Some(hr.is_ok())
            }
        })
        .unwrap_or(false)
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_INSERT {
        let req = unsafe { &*(lp.0 as *const Request) };
        let ok = catch_unwind(|| insert(req)).unwrap_or(false);
        return LRESULT(ok as isize);
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn create_window() -> Option<HWND> {
    unsafe {
        let class = w!("BetterDictationTipWindow");
        let wc = WNDCLASSW { lpfnWndProc: Some(wnd_proc), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc); // already registered by an earlier instance in this process is fine
        CreateWindowExW(WINDOW_EX_STYLE(0), class, w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, None, None).ok()
    }
}

struct SendHwnd(isize);

fn read_exact(pipe: HANDLE, buf: &mut [u8]) -> bool {
    let mut done = 0;
    while done < buf.len() {
        let mut n = 0u32;
        if unsafe { ReadFile(pipe, Some(&mut buf[done..]), Some(&mut n), None) }.is_err() || n == 0 {
            return false;
        }
        done += n as usize;
    }
    true
}

fn write_all(pipe: HANDLE, buf: &[u8]) -> bool {
    let mut n = 0u32;
    unsafe { WriteFile(pipe, Some(buf), Some(&mut n), None) }.is_ok() && n as usize == buf.len()
}

/// Background thread: keeps a connection to the tray app and hands each request to the UI thread.
fn pipe_loop(hwnd: SendHwnd, tid: u32, stop: Arc<AtomicBool>) {
    let hwnd = HWND(hwnd.0 as *mut c_void);
    let pid = std::process::id();
    // BD_TIP_PIPE only exists so the tests can run beside a real tray app.
    let name: Vec<u16> = std::env::var("BD_TIP_PIPE").unwrap_or_else(|_| PIPE.into()).encode_utf16().chain([0]).collect();
    while !stop.load(Ordering::SeqCst) {
        let pipe = unsafe {
            CreateFileW(windows::core::PCWSTR(name.as_ptr()), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_NONE, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None)
        };
        let Ok(pipe) = pipe else {
            for _ in 0..50 {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            continue;
        };
        let mut hello = [0u8; 8];
        hello[..4].copy_from_slice(&pid.to_le_bytes());
        hello[4..].copy_from_slice(&tid.to_le_bytes());
        if write_all(pipe, &hello) {
            loop {
                let mut head = [0u8; 9];
                if !read_exact(pipe, &mut head) {
                    break;
                }
                let erase = i32::from_le_bytes(head[1..5].try_into().unwrap());
                let count = u32::from_le_bytes(head[5..9].try_into().unwrap()) as usize;
                if head[0] != 1 || count > 4096 {
                    break;
                }
                let mut raw = vec![0u8; count * 2];
                if !read_exact(pipe, &mut raw) {
                    break;
                }
                let text = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                let req = Request { erase, text };
                let mut result = 0usize;
                unsafe {
                    SendMessageTimeoutW(
                        hwnd,
                        WM_INSERT,
                        WPARAM(0),
                        LPARAM(&req as *const Request as isize),
                        SMTO_ABORTIFHUNG,
                        1500,
                        Some(&mut result),
                    );
                }
                if !write_all(pipe, &[(result == 1) as u8]) {
                    break;
                }
            }
        }
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(pipe);
        }
    }
}

#[implement(ITfTextInputProcessorEx)]
struct Tip {
    window: RefCell<Option<HWND>>,
    stop: Arc<AtomicBool>,
}

impl ITfTextInputProcessor_Impl for Tip_Impl {
    fn Activate(&self, ptim: Ref<'_, ITfThreadMgr>, tid: u32) -> Result<()> {
        self.ActivateEx(ptim, tid, 0)
    }

    fn Deactivate(&self) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        STATE.with(|s| *s.borrow_mut() = None);
        if let Some(h) = self.window.borrow_mut().take() {
            unsafe {
                let _ = DestroyWindow(h);
            }
        }
        Ok(())
    }
}

impl ITfTextInputProcessorEx_Impl for Tip_Impl {
    fn ActivateEx(&self, ptim: Ref<'_, ITfThreadMgr>, tid: u32, _flags: u32) -> Result<()> {
        catch_unwind(std::panic::AssertUnwindSafe(|| {
            let tm = ptim.ok()?.clone();
            let hwnd = create_window().ok_or(E_FAIL)?;
            STATE.with(|s| *s.borrow_mut() = Some((tm, tid)));
            *self.window.borrow_mut() = Some(hwnd);
            self.stop.store(false, Ordering::SeqCst);
            let (stop, sh) = (self.stop.clone(), SendHwnd(hwnd.0 as isize));
            let thread = unsafe { GetCurrentThreadId() };
            std::thread::spawn(move || {
                let _ = catch_unwind(|| pipe_loop(sh, thread, stop));
            });
            Ok(())
        }))
        .unwrap_or(Err(E_FAIL.into()))
    }
}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Ref<'_, IUnknown>, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        if ppv.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *ppv = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let tip: ITfTextInputProcessorEx = Tip { window: RefCell::new(None), stop: Arc::new(AtomicBool::new(false)) }.into();
        let unknown: IUnknown = tip.cast()?;
        unsafe { unknown.query(riid, ppv).ok() }
    }

    fn LockServer(&self, _lock: windows::core::BOOL) -> Result<()> {
        Ok(())
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(clsid: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    catch_unwind(|| {
        if clsid.is_null() || riid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        unsafe {
            if *clsid != CLSID_TIP {
                return CLASS_E_CLASSNOTAVAILABLE;
            }
            let factory: IClassFactory = Factory.into();
            let unknown: IUnknown = match factory.cast() {
                Ok(u) => u,
                Err(_) => return E_NOINTERFACE,
            };
            unknown.query(riid, ppv)
        }
    })
    .unwrap_or(E_FAIL)
}

/// Never unloaded while a host app runs: the pipe thread and window may still be alive.
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    let _ = S_OK;
    S_FALSE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::windows::io::FromRawHandle;
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
    use windows::Win32::System::LibraryLoader::LoadLibraryW;
    use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_WAIT};
    use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::TextServices::CLSID_TF_ThreadMgr;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, SendMessageW, TranslateMessage, WM_GETTEXT,
        WM_QUIT, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_VISIBLE, ShowWindow, SW_SHOW,
    };

    fn wait_for_text(edit: HWND, want: &str) -> String {
        let mut last = String::new();
        for _ in 0..40 {
            let mut buf = [0u16; 256];
            let n = unsafe { SendMessageW(edit, WM_GETTEXT, Some(WPARAM(buf.len())), Some(LPARAM(buf.as_mut_ptr() as isize))) };
            last = String::from_utf16_lossy(&buf[..n.0 as usize]);
            if last == want {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        last
    }

    /// Manual only (it is #[ignore]d and must stay so): TSF only accepts edits for a focused text box, so this
    /// briefly shows a focused window on the desktop, which the shared test-window rule (KB #51) forbids for
    /// routine runs. Needs a desktop session. Loads the real text service into this process beside a rich edit box,
    /// plays the tray app over a private pipe, and checks the text lands (and is replaced) in the box.
    /// Run with `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn inserts_and_replaces_text_in_a_rich_edit_box() {
        unsafe {
            std::env::set_var("BD_TIP_PIPE", r"\\.\pipe\BetterDictationTipTest");
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
            let _ = LoadLibraryW(w!("Msftedit.dll")).unwrap();
            let top = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("tip test"), WS_OVERLAPPEDWINDOW | WS_VISIBLE, 100, 100, 400, 200, None, None, Some(HINSTANCE::default()), None).unwrap();
            let edit = CreateWindowExW(WINDOW_EX_STYLE(0), w!("RICHEDIT50W"), w!(""), WS_CHILD | WS_VISIBLE, 5, 5, 380, 150, Some(top), None, Some(HINSTANCE::default()), None).unwrap();
            let _ = SendMessageW(edit, 0x04CC, Some(WPARAM(0x10000)), Some(LPARAM(0x10000))); // EM_SETEDITSTYLE: SES_USECTF
            let _ = ShowWindow(top, SW_SHOW);
            // Windows only lets the foreground app take focus, so borrow its input queue for a moment.
            let fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
            let fg_thread = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(fg, None);
            let me = GetCurrentThreadId();
            let _ = windows::Win32::System::Threading::AttachThreadInput(me, fg_thread, true);
            let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(top);
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetActiveWindow(top);
            let _ = SetFocus(Some(edit));
            let _ = windows::Win32::System::Threading::AttachThreadInput(me, fg_thread, false);
            let mut pump = MSG::default();
            for _ in 0..20 {
                while windows::Win32::UI::WindowsAndMessaging::PeekMessageW(&mut pump, None, 0, 0, windows::Win32::UI::WindowsAndMessaging::PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&pump);
                    DispatchMessageW(&pump);
                }
                std::thread::sleep(Duration::from_millis(25));
            }

            let tm: ITfThreadMgr = CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER).unwrap();
            let client = tm.Activate().unwrap();
            let tip: ITfTextInputProcessorEx = Tip { window: RefCell::new(None), stop: Arc::new(AtomicBool::new(false)) }.into();
            tip.ActivateEx(&tm, client, 0).unwrap();

            let ui_thread = GetCurrentThreadId();
            let server = std::thread::spawn(move || {
                let name: Vec<u16> = r"\\.\pipe\BetterDictationTipTest".encode_utf16().chain([0]).collect();
                let h = CreateNamedPipeW(windows::core::PCWSTR(name.as_ptr()), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_WAIT, 1, 4096, 4096, 0, None);
                assert!(!h.is_invalid(), "pipe: {:?}", windows::Win32::Foundation::GetLastError());
                let _ = ConnectNamedPipe(h, None);
                let mut file = std::fs::File::from_raw_handle(h.0);
                let mut hello = [0u8; 8];
                file.read_exact(&mut hello).unwrap();
                let mut send = |erase: i32, text: &str| -> u8 {
                    let units: Vec<u16> = text.encode_utf16().collect();
                    let mut m = vec![1u8];
                    m.extend_from_slice(&erase.to_le_bytes());
                    m.extend_from_slice(&(units.len() as u32).to_le_bytes());
                    for u in units {
                        m.extend_from_slice(&u.to_le_bytes());
                    }
                    file.write_all(&m).unwrap();
                    let mut a = [0u8; 1];
                    file.read_exact(&mut a).unwrap();
                    a[0]
                };
                let first = send(0, "hello wrld");
                let second = send(4, "world again");
                let _ = PostThreadMessageW(ui_thread, WM_QUIT, WPARAM(0), LPARAM(0));
                (first, second)
            });

            // The UI thread has to keep pumping messages: the service hands each edit to it.
            let checker = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
            });
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            checker.join().unwrap();
            let (first, second) = server.join().unwrap();
            let text = wait_for_text(edit, "hello world again");
            assert_eq!((first, second), (1, 1), "service reported failure");
            assert_eq!(text, "hello world again");
            let _ = DestroyWindow(top);
        }
    }
}
