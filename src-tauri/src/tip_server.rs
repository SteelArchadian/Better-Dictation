//! Pipe server for the Better Dictation text service (tip/ crate). Each app that has the text service
//! loaded connects here and says which process and thread it lives on. To type, we ask the connection
//! for the focused app to insert the text through TSF, so no key presses are sent. If nothing
//! answers, the caller falls back to typing keys.

use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, HWND, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId};
use windows::core::{PCWSTR, PWSTR, w};

// Tests use their own pipe so a running Better Dictation can't answer in place of the test's server.
#[cfg(not(test))]
const PIPE: PCWSTR = w!(r"\\.\pipe\BetterDictationTip");
#[cfg(test)]
const PIPE: PCWSTR = w!(r"\\.\pipe\BetterDictationTipTest");
const ANSWER_WITHIN: Duration = Duration::from_millis(600);

type Job = (Vec<u8>, Sender<bool>);

struct Client {
    id: u64,
    pid: u32,
    tid: u32,
    jobs: Sender<Job>,
}

static CLIENTS: Mutex<Vec<Client>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn clients() -> std::sync::MutexGuard<'static, Vec<Client>> {
    CLIENTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// How many apps currently have the text service connected.
pub fn connected() -> usize {
    clients().len()
}

/// Only the signed-in user may connect: another account could otherwise read what is dictated.
fn user_only_descriptor() -> Option<PSECURITY_DESCRIPTOR> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let got = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as *mut _), len, &mut len);
        let _ = CloseHandle(token);
        got.ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut sid = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid).ok()?;
        let text = sid.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(sid.0 as *mut _)));
        let sddl: Vec<u16> = format!("D:(A;;GA;;;{})\0", text?).encode_utf16().collect();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut sd, None).ok()?;
        Some(sd)
    }
}

pub fn start() {
    std::thread::Builder::new().name("tip-server".into()).spawn(serve).ok();
}

fn serve() {
    let Some(sd) = user_only_descriptor() else {
        crate::logger::log("text service pipe: couldn't build the user-only access rule; not serving");
        return;
    };
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    };
    loop {
        let pipe = unsafe {
            CreateNamedPipeW(
                PIPE,
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                Some(&sa),
            )
        };
        if pipe.is_invalid() {
            crate::logger::log("text service pipe: couldn't be created");
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        // An app that connected between create and connect reports ERROR_PIPE_CONNECTED; either way it's connected.
        let _ = unsafe { ConnectNamedPipe(pipe, None) };
        let file = unsafe { File::from_raw_handle(pipe.0) };
        std::thread::spawn(move || client(file));
    }
}

fn client(mut file: File) {
    let mut hello = [0u8; 8];
    if file.read_exact(&mut hello).is_err() {
        return;
    }
    let pid = u32::from_le_bytes(hello[..4].try_into().unwrap());
    let tid = u32::from_le_bytes(hello[4..].try_into().unwrap());
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let (jobs, queue) = mpsc::channel::<Job>();
    clients().push(Client { id, pid, tid, jobs });
    for (bytes, reply) in queue {
        let mut answer = [0u8; 1];
        // A failed edit (answer 0) leaves the connection usable; only a broken pipe ends it.
        if file.write_all(&bytes).is_err() || file.read_exact(&mut answer).is_err() {
            let _ = reply.send(false);
            break;
        }
        let _ = reply.send(answer[0] == 1);
    }
    clients().retain(|c| c.id != id);
}

/// Inserts `text` after erasing `erase` UTF-16 units before the caret, in the window that has focus.
/// Returns false when the focused app has no text service connected or it couldn't do the edit.
pub fn insert(window: isize, erase: usize, text: &str) -> bool {
    let mut pid = 0u32;
    let tid = unsafe { GetWindowThreadProcessId(HWND(window as *mut _), Some(&mut pid)) };
    insert_in(pid, tid, erase, text)
}

fn insert_in(pid: u32, tid: u32, erase: usize, text: &str) -> bool {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut bytes = Vec::with_capacity(9 + units.len() * 2);
    bytes.push(1u8);
    bytes.extend_from_slice(&(erase as i32).to_le_bytes());
    bytes.extend_from_slice(&(units.len() as u32).to_le_bytes());
    for u in units {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    let (jobs, id) = {
        let list = clients();
        let found = list.iter().rev().find(|c| c.pid == pid && c.tid == tid).or_else(|| list.iter().rev().find(|c| c.pid == pid));
        match found {
            Some(c) => (c.jobs.clone(), c.id),
            None => return false,
        }
    };
    let (tx, rx) = mpsc::channel();
    if jobs.send((bytes, tx)).is_err() {
        return false;
    }
    match rx.recv_timeout(ANSWER_WITHIN) {
        Ok(ok) => ok,
        Err(_) => {
            // The app didn't answer in time (hung or gone): stop using this connection.
            clients().retain(|c| c.id != id);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the text service: connects, says hello, answers one request like bd_tip.dll does.
    #[test]
    fn a_connected_service_receives_the_edit_and_its_answer_comes_back() {
        start();
        let mut pipe = loop {
            if let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open(r"\\.\pipe\BetterDictationTipTest") {
                break f;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let (pid, tid) = (4_000_000_001u32, 7u32);
        let mut hello = pid.to_le_bytes().to_vec();
        hello.extend_from_slice(&tid.to_le_bytes());
        pipe.write_all(&hello).unwrap();
        while connected() == 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!insert_in(pid + 1, tid, 0, "x"), "another app has no service");

        let service = std::thread::spawn(move || {
            let mut head = [0u8; 9];
            pipe.read_exact(&mut head).unwrap();
            let erase = i32::from_le_bytes(head[1..5].try_into().unwrap());
            let count = u32::from_le_bytes(head[5..9].try_into().unwrap()) as usize;
            let mut raw = vec![0u8; count * 2];
            pipe.read_exact(&mut raw).unwrap();
            let text: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            pipe.write_all(&[1]).unwrap();
            (head[0], erase, String::from_utf16(&text).unwrap())
        });
        assert!(insert_in(pid, tid, 3, "héllo 😀"));
        assert_eq!(service.join().unwrap(), (1, 3, "héllo 😀".to_string()));
    }
}
