//! The short cue played when dictation turns on. The wavs were picked by the user from the shared soundlab
//! sheet (D:\AI\Claude\Tools\soundlab, rules\ui-sound.md) and are compiled in, so there is nothing to install.

use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::core::PCWSTR;

pub const NAMES: [&str; 3] = ["drop", "drops", "chime"];

fn data(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "drop" => include_bytes!("../sounds/drop.wav"),
        "drops" => include_bytes!("../sounds/drops.wav"),
        "chime" => include_bytes!("../sounds/chime.wav"),
        _ => return None,
    })
}

/// Plays the named cue without blocking; "off" is silent, and a name from an older version plays the default.
pub fn play(name: &str) {
    if name == "off" {
        return;
    }
    if let Some(bytes) = data(name).or_else(|| data("drop")) {
        unsafe {
            let _ = PlaySoundW(PCWSTR(bytes.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_listed_sound_is_a_wav() {
        for name in super::NAMES {
            let bytes = super::data(name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(&bytes[..4], b"RIFF", "{name}");
        }
    }
}
