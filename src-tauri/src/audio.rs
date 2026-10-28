//! Microphone capture on its own thread (cpal streams can't move between threads).
//!
//! While running it reports the voice level to the bubble about 30 times a second. If the device
//! errors (unplugged, switched), the stream is rebuilt once a second until it comes back.
//! Samples are mixed down to mono and passed to the recognizer at the device's own rate
//! (sherpa-onnx resamples internally).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream};
use tauri::{AppHandle, Emitter};

use crate::asr;

enum Cmd {
    Start(Option<String>),
    Stop,
}

pub struct Audio {
    tx: Sender<Cmd>,
}

impl Audio {
    pub fn new(app: AppHandle, sink: Sender<asr::Msg>) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || audio_thread(app, rx, sink))
            .expect("spawn audio thread");
        Audio { tx }
    }

    pub fn start(&self, mic: Option<String>) {
        let _ = self.tx.send(Cmd::Start(mic));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }
}

pub fn list_microphones() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

fn find_device(name: &Option<String>) -> Option<Device> {
    let host = cpal::default_host();
    if let Some(name) = name {
        let found = host
            .input_devices()
            .ok()
            .and_then(|mut it| it.find(|d| d.name().ok().as_deref() == Some(name.as_str())));
        if found.is_some() {
            return found;
        }
        crate::logger::log(&format!("microphone '{name}' not found; using the default"));
    }
    host.default_input_device()
}

/// Peak RMS since the last read, stored as f32 bits.
struct Meter {
    peak: AtomicU32,
    failed: AtomicBool,
}

impl Meter {
    fn feed(&self, rms: f32) {
        let _ = self.peak.try_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            (rms > f32::from_bits(old)).then_some(rms.to_bits())
        });
    }

    fn take(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }
}

/// Builds the per-buffer callback: mix to mono, meter it, hand it to the recognizer.
fn capture<T: Copy>(
    channels: usize,
    rate: u32,
    meter: Arc<Meter>,
    sink: Sender<asr::Msg>,
    to_f32: impl Fn(T) -> f32,
) -> impl FnMut(&[T], &cpal::InputCallbackInfo) {
    move |data, _| {
        let mono: Vec<f32> = data
            .chunks(channels.max(1))
            .map(|frame| frame.iter().map(|&s| to_f32(s)).sum::<f32>() / frame.len() as f32)
            .collect();
        if mono.is_empty() {
            return;
        }
        let sum: f32 = mono.iter().map(|s| s * s).sum();
        meter.feed((sum / mono.len() as f32).sqrt());
        let _ = sink.send(asr::Msg::Samples(rate, mono));
    }
}

fn open(mic: &Option<String>, meter: &Arc<Meter>, sink: &Sender<asr::Msg>) -> Result<Stream, String> {
    let device = find_device(mic).ok_or("no microphone found")?;
    let config = device.default_input_config().map_err(|e| e.to_string())?;
    let format = config.sample_format();
    let cfg: cpal::StreamConfig = config.into();
    let (ch, rate) = (cfg.channels as usize, cfg.sample_rate.0);
    let m = meter.clone();
    let err = move |e| {
        crate::logger::log(&format!("microphone error: {e}"));
        m.failed.store(true, Ordering::SeqCst);
    };
    let (m, s) = (meter.clone(), sink.clone());
    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(&cfg, capture(ch, rate, m, s, |x: f32| x), err, None),
        SampleFormat::I16 => {
            device.build_input_stream(&cfg, capture(ch, rate, m, s, |x: i16| x as f32 / 32768.0), err, None)
        }
        SampleFormat::U16 => device.build_input_stream(
            &cfg,
            capture(ch, rate, m, s, |x: u16| (x as f32 - 32768.0) / 32768.0),
            err,
            None,
        ),
        f => return Err(format!("unsupported sample format {f:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    crate::logger::log(&format!("microphone open: {}", device.name().unwrap_or_default()));
    Ok(stream)
}

/// Map RMS to 0..1 on a log scale (-60 dB is silence, -10 dB is loud speech).
fn level(rms: f32) -> f32 {
    let db = 20.0 * rms.max(1e-6).log10();
    ((db + 60.0) / 50.0).clamp(0.0, 1.0)
}

fn audio_thread(app: AppHandle, rx: Receiver<Cmd>, sink: Sender<asr::Msg>) {
    let meter = Arc::new(Meter { peak: AtomicU32::new(0), failed: AtomicBool::new(false) });
    let mut stream: Option<Stream> = None;
    let mut wanted: Option<Option<String>> = None;
    let mut last_retry = Instant::now();

    loop {
        let timeout = if wanted.is_some() { Duration::from_millis(33) } else { Duration::from_secs(3600) };
        match rx.recv_timeout(timeout) {
            Ok(Cmd::Start(mic)) => {
                stream = None;
                meter.failed.store(false, Ordering::SeqCst);
                match open(&mic, &meter, &sink) {
                    Ok(s) => stream = Some(s),
                    Err(e) => crate::logger::log(&format!("microphone open failed: {e}")),
                }
                last_retry = Instant::now();
                wanted = Some(mic);
            }
            Ok(Cmd::Stop) => {
                stream = None;
                wanted = None;
                meter.take();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        let Some(mic) = &wanted else { continue };
        if meter.failed.load(Ordering::SeqCst) {
            stream = None;
        }
        if stream.is_none() && last_retry.elapsed() >= Duration::from_secs(1) {
            last_retry = Instant::now();
            meter.failed.store(false, Ordering::SeqCst);
            stream = open(mic, &meter, &sink).ok();
        }
        let lvl = if stream.is_some() { level(meter.take()) } else { 0.0 };
        let _ = app.emit_to("bubble", "mic-level", lvl);
    }
}
