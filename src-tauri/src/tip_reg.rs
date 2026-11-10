//! Cleanup for the text service that versions 0.1.3 to 0.1.7 could install (an input method that showed up
//! as a second keyboard). It is no longer shipped. `--tip-remove` is kept so the uninstaller can take a
//! leftover registration away; it runs in an elevated copy of this exe and exits before the tray app starts.

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RegDeleteTreeW};
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, GUID_TFCAT_TIP_KEYBOARD, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT, ITfCategoryMgr, ITfInputProcessorProfileMgr,
};
use windows::core::{GUID, PCWSTR, w};

const CLSID: GUID = GUID::from_u128(0xB7E2C1A4_5D3F_4C8E_9A61_0D2F7B3E8C15);
const PROFILE: GUID = GUID::from_u128(0x4F1D9A27_8B3C_4E6D_A5F0_6C2E81D7B934);
const CLSID_KEY: PCWSTR = w!(r"SOFTWARE\Classes\CLSID\{B7E2C1A4-5D3F-4C8E-9A61-0D2F7B3E8C15}");
const TIP_KEY: PCWSTR = w!(r"SOFTWARE\Microsoft\CTF\TIP\{B7E2C1A4-5D3F-4C8E-9A61-0D2F7B3E8C15}");
const LANG: u16 = 0x0409;

/// Runs in the elevated copy. Missing pieces are fine: the goal is that nothing stays registered.
fn remove_elevated() -> i32 {
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
        let leftover = RegDeleteTreeW(HKEY_LOCAL_MACHINE, TIP_KEY);
        // 2 = key already gone
        if (r.0 == 0 || r.0 == 2) && (leftover.0 == 0 || leftover.0 == 2) { 0 } else { 3 }
    }
}

/// `--tip-remove` runs the elevated cleanup and exits before the tray app starts.
pub fn handle_args() -> Option<i32> {
    match std::env::args().nth(1)?.as_str() {
        "--tip-remove" => Some(remove_elevated()),
        _ => None,
    }
}
