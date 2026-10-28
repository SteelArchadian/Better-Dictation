//! The accuracy pass: the larger Parakeet model re-reads a sentence's audio, every second or so
//! while you talk and once more when the sentence ends. It runs on its own thread (asr.rs).
//!
//! Off never loads the model. Auto keeps running it only while it finishes in under half the
//! sentence's length (checking again every tenth sentence after it falls behind). Always runs it every time.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig};

pub const ACCURATE_MODEL: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8";

pub const OFF: u8 = 0;
pub const AUTO: u8 = 1;
pub const ALWAYS: u8 = 2;
pub static MODE: AtomicU8 = AtomicU8::new(AUTO);

pub struct Refiner {
    recognizer: Option<OfflineRecognizer>,
    tried_load: bool,
    /// Running average of decode time / audio length.
    speed: f32,
    skipped: u32,
}

impl Refiner {
    pub fn new() -> Self {
        Refiner { recognizer: None, tried_load: false, speed: 0.0, skipped: 0 }
    }

    pub fn preload(&mut self) {
        if MODE.load(Ordering::SeqCst) != OFF {
            self.load();
        }
    }

    fn load(&mut self) -> Option<&OfflineRecognizer> {
        if !self.tried_load {
            self.tried_load = true;
            let dir = crate::asr::models_dir().map(|d| d.join(ACCURATE_MODEL)).filter(|d| d.is_dir());
            match dir {
                None => crate::logger::log(&format!("accuracy model folder {ACCURATE_MODEL} not found")),
                Some(dir) => {
                    let file = |name: &str| Some(dir.join(name).to_string_lossy().into_owned());
                    let mut config = OfflineRecognizerConfig::default();
                    config.model_config.transducer.encoder = file("encoder.int8.onnx");
                    config.model_config.transducer.decoder = file("decoder.int8.onnx");
                    config.model_config.transducer.joiner = file("joiner.int8.onnx");
                    config.model_config.tokens = file("tokens.txt");
                    config.model_config.model_type = Some("nemo_transducer".into());
                    config.model_config.num_threads = 4;
                    config.model_config.provider = Some("cpu".into());
                    let started = Instant::now();
                    self.recognizer = OfflineRecognizer::create(&config);
                    crate::logger::log(&match &self.recognizer {
                        Some(_) => format!("accuracy model loaded in {} ms", started.elapsed().as_millis()),
                        None => "accuracy model failed to load".into(),
                    });
                }
            }
        }
        self.recognizer.as_ref()
    }

    /// Re-reads one sentence (`partial`: the sentence so far, while you are still talking).
    /// None when the pass is off, skipped, or unavailable.
    pub fn transcribe(&mut self, rate: u32, samples: &[f32], partial: bool) -> Option<String> {
        let mode = MODE.load(Ordering::SeqCst);
        if mode == OFF || samples.is_empty() {
            return None;
        }
        // Partial passes repeat every second or so, so Auto runs them only on a PC with lots of room.
        if partial && mode == AUTO && self.speed > 0.1 {
            return None;
        }
        if !partial && mode == AUTO && self.speed > 0.5 {
            self.skipped += 1;
            if self.skipped % 10 != 0 {
                return None;
            }
        }
        let r = self.load()?;
        let started = Instant::now();
        let stream = r.create_stream();
        stream.accept_waveform(rate as i32, samples);
        r.decode(&stream);
        let text = stream.get_result()?.text;
        let audio_secs = samples.len() as f32 / rate as f32;
        let ratio = started.elapsed().as_secs_f32() / audio_secs.max(0.1);
        self.speed = if self.speed == 0.0 { ratio } else { self.speed * 0.7 + ratio * 0.3 };
        if !partial {
            crate::logger::log(&format!(
                "accuracy pass: {audio_secs:.1} s of audio in {} ms",
                started.elapsed().as_millis()
            ));
        }
        Some(text)
    }
}
