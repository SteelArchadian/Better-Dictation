//! Installing and removing the text service (tip/ crate, bd_tip.dll). Windows only loads a text
//! service that is registered machine-wide, so install/remove run in a second, elevated copy of this
//! exe (`--tip-install`, `--tip-remove`), started from Settings with one UAC prompt. Turning the
//! profile on for the signed-in user happens afterwards in the normal, non-elevated app.

use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, GUID_TFCAT_TIP_KEYBOARD, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT, ITfCategoryMgr, ITfInputProcessorProfileMgr, TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE,
    TF_IPPMF_ENABLEPROFILE, TF_IPPMF_FORSESSION, TF_PROFILETYPE_INPUTPROCESSOR,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{GUID, HSTRING, PCWSTR, w};

/// Same value as CLSID_TIP in tip/src/lib.rs.
const CLSID: GUID = GUID::from_u128(0xB7E2C1A4_5D3F_4C8E_9A61_0D2F7B3E8C15);
const PROFILE: GUID = GUID::from_u128(0x4F1D9A27_8B3C_4E6D_A5F0_6C2E81D7B934);
const CLSID_KEY: PCWSTR = w!(r"SOFTWARE\Classes\CLSID\{B7E2C1A4-5D3F-4C8E-9A61-0D2F7B3E8C15}");
const INPROC_KEY: PCWSTR = w!(r"SOFTWARE\Classes\CLSID\{B7E2C1A4-5D3F-4C8E-9A61-0D2F7B3E8C15}\InprocServer32");
/// English (United States); the profile keeps the US keyboard layout so typing is unchanged.
const LANG: u16 = 0x0409;
const US_LAYOUT: HKL = HKL(0x0409_0409 as *mut _);

fn dll_path() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dll = exe.with_file_name("bd_tip.dll");
    if dll.is_file() { Ok(dll) } else { Err(format!("{} is missing", dll.display())) }
}

/// Registered with Windows (machine-wide) and pointing at a file that exists.
pub fn installed() -> bool {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, CLSID_KEY, None, KEY_READ, &mut key) != ERROR_SUCCESS {
            return false;
        }
        let _ = RegCloseKey(key);
        true
    }
}

fn set_default(key: HKEY, name: PCWSTR, value: &str) -> Result<(), String> {
    let wide = HSTRING::from(value);
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, (wide.len() + 1) * 2) };
    let r = unsafe { RegSetValueExW(key, name, None, REG_SZ, Some(bytes)) };
    if r == ERROR_SUCCESS { Ok(()) } else { Err(format!("registry error {}", r.0)) }
}

fn write_com_key(dll: &str) -> Result<(), String> {
    unsafe {
        let mut key = HKEY::default();
        let r = RegCreateKeyExW(HKEY_LOCAL_MACHINE, INPROC_KEY, None, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut key, None);
        if r != ERROR_SUCCESS {
            return Err(format!("couldn't write the COM entry (error {}); run as administrator", r.0));
        }
        let result = set_default(key, PCWSTR::null(), dll).and_then(|_| set_default(key, w!("ThreadingModel"), "Apartment"));
        let _ = RegCloseKey(key);
        result
    }
}

/// Runs in the elevated copy. Exit code 0 on success.
pub fn install_elevated() -> i32 {
    let dll = match dll_path() {
        Ok(d) => d,
        Err(_) => return 2,
    };
    if write_com_key(&dll.to_string_lossy()).is_err() {
        return 3;
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let code = (|| -> i32 {
            let Ok(profiles) = CoCreateInstance::<_, ITfInputProcessorProfileMgr>(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER) else {
                return 4;
            };
            let name: Vec<u16> = "Better Dictation".encode_utf16().collect();
            let icon: Vec<u16> = std::env::current_exe().map(|p| p.to_string_lossy().encode_utf16().collect()).unwrap_or_default();
            if profiles.RegisterProfile(&CLSID, LANG, &PROFILE, &name, &icon, 0, US_LAYOUT, 0, true, 0).is_err() {
                return 4;
            }
            let Ok(cats) = CoCreateInstance::<_, ITfCategoryMgr>(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) else {
                return 5;
            };
            for cat in [GUID_TFCAT_TIP_KEYBOARD, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT, GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT] {
                if cats.RegisterCategory(&CLSID, &cat, &CLSID).is_err() {
                    return 5;
                }
            }
            0
        })();
        CoUninitialize();
        code
    }
}

/// Runs in the elevated copy. Missing pieces are fine: the goal is that nothing stays registered.
pub fn remove_elevated() -> i32 {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if let Ok(cats) = CoCreateInstance::<_, ITfCategoryMgr>(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) {
            for cat in [GUID_TFCAT_TIP_KEYBOARD, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT, GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT] {
                let _ = cats.UnregisterCategory(&CLSID, &cat, &CLSID);
            }
        }
        if let Ok(profiles) = CoCreateInstance::<_, ITfInputProcessorProfileMgr>(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER) {
            let _ = profiles.UnregisterProfile(&CLSID, LANG, &PROFILE, 0);
        }
        CoUninitialize();
        let r = RegDeleteTreeW(HKEY_LOCAL_MACHINE, CLSID_KEY);
        // 2 = key already gone
        if r == ERROR_SUCCESS || r.0 == 2 { 0 } else { 3 }
    }
}

/// Turns the profile on for the signed-in user (non-elevated), so Windows loads the service into apps.
fn activate() -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = CoCreateInstance::<_, ITfInputProcessorProfileMgr>(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| e.to_string())
            .and_then(|profiles| {
                profiles
                    .ActivateProfile(
                        TF_PROFILETYPE_INPUTPROCESSOR,
                        LANG,
                        &CLSID,
                        &PROFILE,
                        US_LAYOUT,
                        TF_IPPMF_ENABLEPROFILE | TF_IPPMF_FORSESSION | TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE,
                    )
                    .map_err(|e| e.to_string())
            });
        CoUninitialize();
        result
    }
}

/// Starts this exe again with a UAC prompt and waits for it.
fn run_elevated(arg: &str) -> Result<i32, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(arg);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).map_err(|_| "the administrator prompt was cancelled".to_string())?;
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        Ok(code as i32)
    }
}

fn explain(code: i32) -> String {
    match code {
        2 => "bd_tip.dll is missing from the app folder".into(),
        3 => "couldn't write the registry entry".into(),
        4 => "Windows refused the input method profile".into(),
        5 => "Windows refused the input method categories".into(),
        c => format!("failed (code {c})"),
    }
}

pub fn install() -> Result<(), String> {
    dll_path()?;
    match run_elevated("--tip-install")? {
        0 => activate(),
        c => Err(explain(c)),
    }
}

pub fn remove() -> Result<(), String> {
    match run_elevated("--tip-remove")? {
        0 => Ok(()),
        c => Err(explain(c)),
    }
}

/// `--tip-install` / `--tip-remove` run the elevated half and exit before the tray app starts.
pub fn handle_args() -> Option<i32> {
    let arg = std::env::args().nth(1)?;
    match arg.as_str() {
        "--tip-install" => Some(install_elevated()),
        "--tip-remove" => Some(remove_elevated()),
        _ => None,
    }
}
