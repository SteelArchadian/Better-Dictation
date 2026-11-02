//! Start with Windows: one value in the current user's Run key (no admin needed), pointing at this exe.
//! The app opens straight into the tray, since it has no window of its own at start.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR, w};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const NAME: PCWSTR = w!("Better Dictation");

pub fn set(enabled: bool) -> Result<(), String> {
    unsafe {
        let mut key = HKEY::default();
        let opened = RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE, &mut key);
        if opened != ERROR_SUCCESS {
            return Err(format!("couldn't open the startup list (error {})", opened.0));
        }
        let result = if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let command = HSTRING::from(format!("\"{}\"", exe.display()));
            let bytes = std::slice::from_raw_parts(command.as_ptr() as *const u8, (command.len() + 1) * 2);
            RegSetValueExW(key, NAME, None, REG_SZ, Some(bytes))
        } else {
            let r = RegDeleteValueW(key, NAME);
            // Already absent is the state we wanted.
            if r.0 == 2 { ERROR_SUCCESS } else { r }
        };
        let _ = RegCloseKey(key);
        if result == ERROR_SUCCESS { Ok(()) } else { Err(format!("Windows error {}", result.0)) }
    }
}

#[cfg(test)]
mod tests {
    /// Touches the real Run key (adds then removes the entry), so it only runs on request:
    /// `cargo test -- --ignored autostart`
    #[test]
    #[ignore]
    fn adds_and_removes_the_run_entry() {
        let present = || {
            let out = std::process::Command::new("reg")
                .args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "Better Dictation"])
                .output()
                .unwrap();
            out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
        };
        assert!(present().is_none(), "an entry already exists; not touching it");
        super::set(true).unwrap();
        let listed = present().expect("entry missing after set(true)");
        assert!(listed.contains("better_dictation") || listed.contains("better-dictation"), "{listed}");
        assert!(listed.contains('"'), "path should be quoted: {listed}");
        super::set(false).unwrap();
        assert!(present().is_none());
        super::set(false).unwrap();
    }
}
