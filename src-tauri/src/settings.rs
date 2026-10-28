//! User settings, stored as JSON in the app's config folder.
//!
//! Saves go through a temp file and a rename so a crash mid-write never leaves a torn file.
//! A file that fails to parse is kept aside (settings.json.bad) rather than overwritten.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyNameTextW, MAPVK_VK_TO_VSC, MapVirtualKeyW};

pub const MOD_WIN: u32 = 1;
pub const MOD_CTRL: u32 = 2;
pub const MOD_ALT: u32 = 4;
pub const MOD_SHIFT: u32 = 8;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Shortcut {
    pub vk: u32,
    pub mods: u32,
}

impl Default for Shortcut {
    fn default() -> Self {
        Shortcut { vk: 0x48, mods: MOD_WIN } // Win+H
    }
}

impl Shortcut {
    pub fn pack(self) -> u32 {
        (self.mods << 16) | (self.vk & 0xFFFF)
    }

    pub fn unpack(v: u32) -> Self {
        Shortcut { vk: v & 0xFFFF, mods: v >> 16 }
    }

    pub fn label(self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (bit, name) in [(MOD_WIN, "Win"), (MOD_CTRL, "Ctrl"), (MOD_ALT, "Alt"), (MOD_SHIFT, "Shift")] {
            if self.mods & bit != 0 {
                parts.push(name.into());
            }
        }
        parts.push(key_name(self.vk));
        parts.join(" + ")
    }
}

fn key_name(vk: u32) -> String {
    unsafe {
        let mut scan = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC) as i32;
        // Navigation keys share scan codes with the numpad; the extended bit picks the right name.
        if matches!(vk, 0x21..=0x2E | 0x5B | 0x5C | 0x6F) {
            scan |= 0x100;
        }
        let mut buf = [0u16; 64];
        let n = GetKeyNameTextW(scan << 16, &mut buf);
        if n > 0 {
            String::from_utf16_lossy(&buf[..n as usize])
        } else {
            format!("Key {vk:#04X}")
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    /// The shortcut is active. With the default Win+H this replaces Windows dictation.
    pub enabled: bool,
    pub shortcut: Shortcut,
    /// Input device name; None follows the Windows default microphone.
    pub microphone: Option<String>,
    /// Accuracy pass: "off", "auto" (only while the PC keeps up) or "always".
    pub accuracy: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: true, shortcut: Shortcut::default(), microphone: None, accuracy: "auto".into() }
    }
}

pub fn accuracy_mode(name: &str) -> u8 {
    match name {
        "off" => crate::refine::OFF,
        "always" => crate::refine::ALWAYS,
        _ => crate::refine::AUTO,
    }
}

pub fn path(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

pub fn load(path: &Path) -> Settings {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Settings::default(),
    };
    match serde_json::from_str(&text) {
        Ok(s) => s,
        Err(e) => {
            let bad = path.with_extension("json.bad");
            crate::logger::log(&format!("settings unreadable ({e}); kept as {}", bad.display()));
            let _ = std::fs::copy(path, &bad);
            Settings::default()
        }
    }
}

pub fn save(path: &Path, s: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(s)?)?;
    std::fs::rename(&tmp, path)
}
