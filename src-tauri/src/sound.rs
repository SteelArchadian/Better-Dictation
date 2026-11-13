//! The short cue played when dictation turns on. The wavs were picked by the user from the shared soundlab
//! sheet (D:\AI\Claude\Tools\soundlab, rules\ui-sound.md) and are compiled in, so there is nothing to install.

use std::sync::Mutex;

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

/// The scaled copy now playing; PlaySound reads straight from memory, so it has to stay alive until the next cue replaces it.
static PLAYING: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// The wav with every 16-bit sample scaled to `percent` of its level (header untouched).
fn scaled(bytes: &[u8], percent: u8) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let start = out.windows(4).position(|w| w == b"data").map_or(out.len(), |i| i + 8);
    let gain = f32::from(percent.min(100)) / 100.0;
    for pair in out[start..].chunks_exact_mut(2) {
        let v = (f32::from(i16::from_le_bytes([pair[0], pair[1]])) * gain) as i16;
        pair.copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// The wav played backwards (whole sample frames swapped end to end, header untouched).
fn reversed(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let start = out.windows(4).position(|w| w == b"data").map_or(out.len(), |i| i + 8);
    let channels = usize::from(u16::from_le_bytes([out[22], out[23]]).max(1));
    let frame = 2 * channels;
    let n = (out.len() - start) / frame;
    for i in 0..n / 2 {
        for k in 0..frame {
            out.swap(start + i * frame + k, start + (n - 1 - i) * frame + k);
        }
    }
    out
}

/// Plays the named cue without blocking at `volume` percent; "off" is silent, and a name from an older version plays the default.
pub fn play(name: &str, volume: u8) {
    play_with(name, volume, false);
}

/// The same cue backwards, for when dictation turns off.
pub fn play_reversed(name: &str, volume: u8) {
    play_with(name, volume, true);
}

fn play_with(name: &str, volume: u8, backwards: bool) {
    if name == "off" || volume == 0 {
        return;
    }
    if let Some(bytes) = data(name).or_else(|| data("drop")) {
        let mut playing = PLAYING.lock().unwrap_or_else(|e| e.into_inner());
        *playing = scaled(&if backwards { reversed(bytes) } else { bytes.to_vec() }, volume);
        unsafe {
            let _ = PlaySoundW(PCWSTR(playing.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn volume_scales_samples_not_header() {
        let wav = super::data("drop").unwrap();
        let half = super::scaled(wav, 50);
        assert_eq!(&half[..44], &wav[..44]);
        let (a, b) = (i16::from_le_bytes([wav[1000], wav[1001]]), i16::from_le_bytes([half[1000], half[1001]]));
        assert_eq!(b, a / 2);
    }

    #[test]
    fn every_listed_sound_is_a_wav() {
        for name in super::NAMES {
            let bytes = super::data(name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(&bytes[..4], b"RIFF", "{name}");
        }
    }
}
