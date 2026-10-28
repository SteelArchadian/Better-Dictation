//! Live speech recognition on its own thread.
//!
//! The streaming model keeps revising its last few words as more audio arrives. A word is typed
//! only once it has stayed the same for STABLE and is not the newest word. At a pause (endpoint)
//! or stop, everything left is typed.
//!
//! The accuracy pass runs beside it on a second thread: about every second it re-reads the
//! sentence so far, and once more when the sentence ends. Fixes land close to the caret, and no fix
//! ever erases more than MAX_REWIND characters, so the text keeps flowing instead of rewinding.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineStream};

use crate::numbers;
use crate::refine::{self, Refiner};
use crate::typing;

const LIVE_MODEL: &str = "sherpa-onnx-streaming-zipformer-en-kroko-2025-08-06";
const STABLE: Duration = Duration::from_millis(400);
/// The most text one correction may erase and retype. Differences further back stay as typed.
const MAX_REWIND: usize = 40;
/// How much new audio (in tenths of a second) starts another re-read of the sentence so far.
const PARTIAL_EVERY: usize = 12;
/// After Esc or the shortcut, how long to wait for the last fix. Later than this the user may
/// already be typing, and a fix would land among their words.
const FINAL_WAIT: Duration = Duration::from_secs(2);

static NEXT_SEGMENT: AtomicU64 = AtomicU64::new(1);

pub enum Msg {
    Begin,
    Samples(u32, Vec<f32>),
    /// `clean`: stopped by Esc or the shortcut, which never reach the app.
    End { clean: bool },
}

pub fn start() -> Sender<Msg> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("asr".into())
        .spawn(move || asr_thread(rx))
        .expect("spawn asr thread");
    tx
}

/// models\ next to the exe (installed), or in a parent folder (development builds).
pub fn models_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().skip(1).map(|d| d.join("models")).find(|d| d.join(LIVE_MODEL).is_dir())
}

fn load() -> Option<OnlineRecognizer> {
    let Some(dir) = models_dir().map(|d| d.join(LIVE_MODEL)) else {
        crate::logger::log(&format!("live model folder {LIVE_MODEL} not found"));
        return None;
    };
    let file = |name: &str| Some(dir.join(name).to_string_lossy().into_owned());
    let mut config = OnlineRecognizerConfig::default();
    config.model_config.transducer.encoder = file("encoder.onnx");
    config.model_config.transducer.decoder = file("decoder.onnx");
    config.model_config.transducer.joiner = file("joiner.onnx");
    config.model_config.tokens = file("tokens.txt");
    config.model_config.num_threads = 2;
    config.model_config.provider = Some("cpu".into());
    config.decoding_method = Some("greedy_search".into());
    config.enable_endpoint = true;
    config.rule1_min_trailing_silence = 2.4;
    config.rule2_min_trailing_silence = 0.8;
    config.rule3_min_utterance_length = 20.0;
    let started = Instant::now();
    let r = OnlineRecognizer::create(&config);
    crate::logger::log(&match &r {
        Some(_) => format!("live model loaded in {} ms", started.elapsed().as_millis()),
        None => "live model failed to load".into(),
    });
    r
}

/// Applies capitalisation and spacing across the whole dictation session.
#[derive(Default, Clone)]
struct Writer {
    typed_any: bool,
    cap_next: bool,
}

impl Writer {
    /// Each word as it is typed, with its leading space. With `digits`, clear spoken numbers become
    /// digits; the words folded into a number get empty text.
    fn tokens(&mut self, words: &[String], digits: bool) -> Vec<String> {
        let numbers = if digits { numbers::digits(words) } else { vec![None; words.len()] };
        let mut out = Vec::new();
        for (w, number) in words.iter().zip(numbers) {
            if number.as_deref() == Some("") {
                out.push(String::new());
                continue;
            }
            let mut w = number.clone().unwrap_or_else(|| w.clone());
            if number.is_none() && (self.cap_next || is_pronoun_i(&w)) {
                w = capitalise(&w);
            }
            self.cap_next = w.ends_with(['.', '?', '!']);
            let mut t = String::new();
            if self.typed_any && !w.starts_with([',', '.', '?', '!']) {
                t.push(' ');
            }
            t.push_str(&w);
            out.push(t);
            self.typed_any = true;
        }
        out
    }

    #[cfg(test)]
    fn format(&mut self, words: &[String]) -> String {
        self.tokens(words, false).concat()
    }
}

fn is_pronoun_i(w: &str) -> bool {
    let core = w.trim_end_matches([',', '.', '?', '!']);
    core == "i" || core.starts_with("i'")
}

fn capitalise(w: &str) -> String {
    let mut c = w.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn words_of(text: &str) -> Vec<String> {
    // Some models answer in all caps; dictation reads better in normal case.
    let text = if text.chars().any(char::is_lowercase) { text.to_string() } else { text.to_lowercase() };
    text.split_whitespace().map(str::to_string).collect()
}

/// Where the already-typed words end in the model's new guess. The model sometimes rewrites
/// earlier words ("sea sell" becomes "seashells"), so match on content: pick the cut in `hyp` whose
/// prefix is closest to what was typed, so nothing is typed twice or skipped. Letters are compared
/// with spaces and punctuation removed, since merges and splits change the word count.
fn aligned_end(typed: &[String], hyp: &[String]) -> usize {
    if hyp.starts_with(typed) {
        return typed.len();
    }
    let letters = |w: &String| -> Vec<char> {
        w.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
    };
    let t: Vec<char> = typed.iter().flat_map(letters).collect();
    let mut h: Vec<char> = Vec::new();
    let mut ends = vec![0];
    for w in hyp {
        h.extend(letters(w));
        ends.push(h.len());
    }
    // row[k] = edit distance between all of `t` and h[..k].
    let mut row: Vec<usize> = (0..=h.len()).collect();
    for (i, tc) in t.iter().enumerate() {
        let mut next = vec![i + 1; h.len() + 1];
        for (k, hc) in h.iter().enumerate() {
            next[k + 1] = (row[k] + usize::from(tc != hc)).min(row[k + 1] + 1).min(next[k] + 1);
        }
        row = next;
    }
    (0..=hyp.len()).min_by_key(|&j| (row[ends[j]], j.abs_diff(typed.len()))).unwrap_or(0)
}

/// Where typed text goes: the focused window, or a simulated screen in tests.
trait Output {
    fn focused(&self, window: isize) -> bool;
    fn type_text(&mut self, text: &str);
    /// `old` is right before the caret; turn it into `new`.
    fn replace(&mut self, old: &str, new: &str);
}

struct Keyboard;

impl Output for Keyboard {
    fn focused(&self, window: isize) -> bool {
        typing::foreground() == window
    }
    fn type_text(&mut self, text: &str) {
        typing::type_text(text);
    }
    fn replace(&mut self, old: &str, new: &str) {
        typing::replace(old, new);
    }
}

/// One word on screen, as the model wrote it and as it was typed.
struct Token {
    raw: String,
    text: String,
    segment: u64,
}

/// A request to re-read a sentence's audio. `upto`: for a partial pass, how many of the
/// sentence's words were on screen when it was sent.
struct Job {
    segment: u64,
    rate: u32,
    audio: Vec<f32>,
    upto: Option<usize>,
}

struct Fix {
    segment: u64,
    upto: Option<usize>,
    words: Option<Vec<String>>,
}

fn refine_thread(jobs: Receiver<Job>, fixes: Sender<Fix>) {
    let mut refiner = Refiner::new();
    refiner.preload();
    for job in jobs {
        let words = refiner.transcribe(job.rate, &job.audio, job.upto.is_some()).map(|t| words_of(&t));
        if fixes.send(Fix { segment: job.segment, upto: job.upto, words }).is_err() {
            return;
        }
    }
}

struct Session {
    stream: OnlineStream,
    began: Instant,
    /// Sample rate the stream has been fed; None until the first audio arrives.
    rate: Option<u32>,
    window: isize,
    out: Box<dyn Output>,
    writer: Writer,
    /// The model's words (as it had them then) that are already typed for this segment.
    typed: Vec<String>,
    prev: Vec<String>,
    /// When each word of `prev` last changed.
    since: Vec<Instant>,
    /// Everything typed this session, word by word, so a fix knows exactly what to erase.
    screen: Vec<Token>,
    segment: u64,
    /// This sentence's audio, for the accuracy pass, and its length at the last partial pass.
    seg_audio: Vec<f32>,
    partial_at: usize,
    /// The last partial re-read's words, for the two-in-a-row rule.
    last_partial: (u64, Vec<String>),
    /// Something we meant to type was dropped, so the screen no longer matches what we track
    /// and corrections are unsafe for the rest of the session.
    broken: bool,
}

impl Session {
    fn new(stream: OnlineStream, out: Box<dyn Output>, window: isize) -> Self {
        Session {
            stream,
            rate: None,
            window,
            out,
            began: Instant::now(),
            writer: Writer { typed_any: false, cap_next: true },
            typed: Vec::new(),
            prev: Vec::new(),
            since: Vec::new(),
            screen: Vec::new(),
            segment: NEXT_SEGMENT.fetch_add(1, Ordering::SeqCst),
            seg_audio: Vec::new(),
            partial_at: 0,
            last_partial: (0, Vec::new()),
            broken: false,
        }
    }

    /// Types whatever has settled.
    fn update(&mut self, text: &str, now: Instant, flush: bool) {
        let out = self.step(text, now, flush);
        self.type_out(out);
    }

    fn add_audio(&mut self, samples: &[f32], rate: u32) {
        // Segments end after 20 s of speech anyway; the cap only guards memory.
        if self.seg_audio.len() < rate as usize * 30 {
            self.seg_audio.extend_from_slice(samples);
        }
    }

    fn words_in(&self, segment: u64) -> usize {
        self.screen.iter().filter(|t| t.segment == segment).count()
    }

    /// Time for another re-read of the sentence so far?
    fn partial_job(&mut self) -> Option<Job> {
        let rate = self.rate?;
        let upto = self.words_in(self.segment);
        let due = self.partial_at + rate as usize * PARTIAL_EVERY / 10;
        if refine::MODE.load(Ordering::SeqCst) == refine::OFF || self.broken || upto < 3 || self.seg_audio.len() < due {
            return None;
        }
        self.partial_at = self.seg_audio.len();
        Some(Job { segment: self.segment, rate, audio: self.seg_audio.clone(), upto: Some(upto) })
    }

    /// The sentence is complete: start the next one, and hand back its final re-read.
    fn next_segment(&mut self) -> Option<Job> {
        self.typed.clear();
        self.prev.clear();
        self.since.clear();
        let segment = std::mem::replace(&mut self.segment, NEXT_SEGMENT.fetch_add(1, Ordering::SeqCst));
        let audio = std::mem::take(&mut self.seg_audio);
        self.partial_at = 0;
        let rate = self.rate?;
        (self.words_in(segment) > 0 && !self.broken).then_some(Job { segment, rate, audio, upto: None })
    }

    /// Capitalisation and spacing for a word typed at screen position `i`.
    fn writer_at(&self, i: usize) -> Writer {
        match self.screen[..i].iter().rev().find(|t| !t.text.is_empty()) {
            None => Writer { typed_any: false, cap_next: true },
            Some(t) => Writer { typed_any: true, cap_next: t.text.ends_with(['.', '?', '!']) },
        }
    }

    /// Puts the accuracy pass's reading of a sentence on screen, if it differs.
    fn apply(&mut self, fix: Fix) {
        let Some(mut words) = fix.words else { return };
        if words.is_empty() || self.broken || !self.out.focused(self.window) {
            // Unsure beats destructive: never erase a sentence because the second pass heard nothing.
            return;
        }
        let Some(a) = self.screen.iter().position(|t| t.segment == fix.segment) else { return };
        let mut e = a + self.words_in(fix.segment);
        if let Some(upto) = fix.upto {
            // The sentence so far: its last word may be cut off mid-word, and the newest typed
            // word may still be settling, so leave both to later passes. A number still being
            // spoken ("twenty" before "five") waits too.
            words.pop();
            while words.last().is_some_and(|w| numbers::is_number_word(w)) {
                words.pop();
            }
            // On a hard accent the model can hear a phrase differently on every re-read. Change
            // only the words two re-reads in a row agree on, so correct text never flips to wrong.
            let (seg, prev) = std::mem::replace(&mut self.last_partial, (fix.segment, words.clone()));
            let agreed = if seg == fix.segment { prev.iter().zip(&words).take_while(|(x, y)| x == y).count() } else { 0 };
            words.truncate(agreed);
            let n = upto.min(e - a).saturating_sub(1);
            let typed: Vec<String> = self.screen[a..a + n].iter().map(|t| t.raw.clone()).collect();
            words.truncate(aligned_end(&typed, &words));
            e = a + aligned_end(&words, &typed);
        }
        if e <= a || words.is_empty() {
            return;
        }

        let render = |start: usize, words: &[String]| {
            let texts = self.writer_at(start).tokens(words, true);
            let old: String = self.screen[start..].iter().map(|t| t.text.as_str()).collect();
            let new: String =
                texts.iter().map(String::as_str).chain(self.screen[e..].iter().map(|t| t.text.as_str())).collect();
            (texts, old, new)
        };
        let erased = |old: &str, new: &str| {
            old.chars().count() - old.chars().zip(new.chars()).take_while(|(x, y)| x == y).count()
        };
        let mut start = a;
        let (mut texts, mut old, mut new) = render(a, &words);
        if erased(&old, &new) > MAX_REWIND {
            // Fixing this far back would mean a long visible rewind: keep the older words as typed
            // and fix only the last stretch.
            let tail = |i: usize| self.screen[i..].iter().map(|t| t.text.chars().count()).sum::<usize>();
            // Never start inside a number ("thirty five" typed as "35").
            let Some(s) = (a..e).find(|&i| tail(i) <= MAX_REWIND && !self.screen[i].text.is_empty()) else { return };
            let kept: Vec<String> = self.screen[a..s].iter().map(|t| t.raw.clone()).collect();
            words.drain(..aligned_end(&kept, &words));
            if words.is_empty() {
                return;
            }
            start = s;
            (texts, old, new) = render(start, &words);
        }
        if old == new {
            return;
        }
        crate::logger::log(&format!("accuracy pass changed {old:?} -> {new:?}"));
        self.out.replace(&old, &new);
        let segment = fix.segment;
        let fixed = words.into_iter().zip(texts).map(|(raw, text)| Token { raw, text, segment });
        self.screen.splice(start..e, fixed);
        self.writer = self.writer_at(self.screen.len());
    }

    /// Takes the model's latest guess for this segment; returns the words to type now (as the
    /// model wrote them, and as typed).
    fn step(&mut self, text: &str, now: Instant, flush: bool) -> Vec<(String, String)> {
        let words = words_of(text);
        let same = self.prev.iter().zip(&words).take_while(|(a, b)| a == b).count();
        self.since.truncate(same);
        self.since.resize(words.len(), now);

        let start = aligned_end(&self.typed, &words);
        let mut n = start;
        if flush {
            n = words.len();
        } else {
            while n + 1 < words.len() && now - self.since[n] >= STABLE {
                n += 1;
            }
        }
        let mut out = Vec::new();
        if n > start {
            let texts = self.writer.tokens(&words[start..n], false);
            out = words[start..n].iter().cloned().zip(texts).collect();
            self.typed.truncate(0);
            self.typed.extend_from_slice(&words[..n]);
        }
        self.prev = words;
        out
    }

    fn type_out(&mut self, words: Vec<(String, String)>) {
        if words.is_empty() {
            return;
        }
        let text: String = words.iter().map(|(_, t)| t.as_str()).collect();
        // Never type into a window the user didn't start dictating in.
        if !self.out.focused(self.window) {
            crate::logger::log(&format!("focus moved; dropped {} chars", text.len()));
            self.broken = true;
            return;
        }
        self.out.type_text(&text);
        let segment = self.segment;
        self.screen.extend(words.into_iter().map(|(raw, text)| Token { raw, text, segment }));
    }
}

/// The model needs a little trailing audio to settle its last word; without it the end is cut off.
/// It must use the rate the stream already saw (sherpa-onnx aborts on a rate change).
fn finish(stream: &OnlineStream, rate: u32) {
    stream.accept_waveform(rate as i32, &vec![0.0; rate as usize / 2]);
    stream.input_finished();
}

fn drain(r: &OnlineRecognizer, stream: &OnlineStream) {
    while r.is_ready(stream) {
        r.decode(stream);
    }
}

fn result(r: &OnlineRecognizer, stream: &OnlineStream) -> String {
    r.get_result(stream).map(|x| x.text).unwrap_or_default()
}

fn asr_thread(rx: Receiver<Msg>) {
    let (jobs, job_rx) = mpsc::channel();
    let (fix_tx, fixes) = mpsc::channel();
    std::thread::Builder::new()
        .name("refine".into())
        .spawn(move || refine_thread(job_rx, fix_tx))
        .expect("spawn refine thread");
    // Load up front so the first Win+H starts instantly.
    let recognizer = load();
    let mut session: Option<Session> = None;
    // Jobs sent to the accuracy pass and not yet answered (across sessions).
    let mut pending = 0usize;
    let submit = |job: Option<Job>, pending: &mut usize| {
        if job.is_some_and(|job| jobs.send(job).is_ok()) {
            *pending += 1;
        }
    };

    for msg in rx {
        let Some(r) = &recognizer else { continue };
        match msg {
            Msg::Begin => {
                session = Some(Session::new(r.create_stream(), Box::new(Keyboard), typing::foreground()))
            }
            Msg::Samples(rate, samples) => {
                let Some(s) = session.as_mut() else { continue };
                while let Ok(fix) = fixes.try_recv() {
                    pending -= 1;
                    s.apply(fix);
                }
                // A different microphone mid-session can bring a new rate: close out the
                // current stream and continue on a fresh one.
                if let Some(old) = s.rate.filter(|&old| old != rate) {
                    finish(&s.stream, old);
                    drain(r, &s.stream);
                    s.update(&result(r, &s.stream), Instant::now(), true);
                    submit(s.next_segment(), &mut pending);
                    s.stream = r.create_stream();
                }
                if s.rate.is_none() {
                    crate::logger::log(&format!("first audio {} ms after start", s.began.elapsed().as_millis()));
                }
                s.rate = Some(rate);
                s.stream.accept_waveform(rate as i32, &samples);
                s.add_audio(&samples, rate);
                drain(r, &s.stream);
                let endpoint = r.is_endpoint(&s.stream);
                s.update(&result(r, &s.stream), Instant::now(), endpoint);
                if endpoint {
                    r.reset(&s.stream);
                    submit(s.next_segment(), &mut pending);
                } else if pending == 0 {
                    submit(s.partial_job(), &mut pending);
                }
            }
            Msg::End { clean } => {
                let Some(mut s) = session.take() else { continue };
                // A key or click that stopped dictation has already reached the app, so typing
                // or correcting now would land after it, in the wrong place. Only Esc or the
                // shortcut (both swallowed) finish the sentence.
                if !clean {
                    continue;
                }
                if let Some(rate) = s.rate {
                    finish(&s.stream, rate);
                    drain(r, &s.stream);
                    s.update(&result(r, &s.stream), Instant::now(), true);
                    submit(s.next_segment(), &mut pending);
                }
                let deadline = Instant::now() + FINAL_WAIT;
                while pending > 0 {
                    let left = deadline.saturating_duration_since(Instant::now());
                    match fixes.recv_timeout(left) {
                        Ok(fix) => {
                            pending -= 1;
                            s.apply(fix);
                        }
                        Err(_) => {
                            crate::logger::log("accuracy pass too slow; last fix skipped");
                            break;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn formats_sentences() {
        let mut w = Writer { typed_any: false, cap_next: true };
        assert_eq!(w.format(&words("hello there. i'm here")), "Hello there. I'm here");
        assert_eq!(w.format(&words("and i agree")), " and I agree");
    }

    fn words(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn rewritten_words_are_not_typed_twice() {
        // Typed "she sells sea sell", then the model merges the last two words.
        assert_eq!(aligned_end(&words("she sells sea sell"), &words("she sells seashells by")), 3);
        assert_eq!(aligned_end(&words("she sells"), &words("she sells seashells")), 2);
        assert_eq!(aligned_end(&[], &words("hello")), 0);
    }

    /// A simulated text box. `live` collects only live typing; `screen` also takes fixes.
    #[derive(Default)]
    struct Sim {
        live: String,
        screen: String,
        longest_rewind: usize,
    }

    struct SimOut(Rc<RefCell<Sim>>);

    impl Output for SimOut {
        fn focused(&self, _: isize) -> bool {
            true
        }
        fn type_text(&mut self, text: &str) {
            let mut s = self.0.borrow_mut();
            s.live += text;
            s.screen += text;
        }
        fn replace(&mut self, old: &str, new: &str) {
            let mut s = self.0.borrow_mut();
            assert!(s.screen.ends_with(old), "fix target {old:?} is not before the caret in {:?}", s.screen);
            let same = old.chars().zip(new.chars()).take_while(|(a, b)| a == b).count();
            s.longest_rewind = s.longest_rewind.max(old.chars().count() - same);
            let keep = s.screen.len() - old.len();
            s.screen.truncate(keep);
            s.screen += new;
        }
    }

    fn sim_session(r: &OnlineRecognizer) -> (Session, Rc<RefCell<Sim>>) {
        let sim = Rc::new(RefCell::new(Sim::default()));
        (Session::new(r.create_stream(), Box::new(SimOut(sim.clone())), 0), sim)
    }

    #[test]
    #[ignore]
    fn long_sentence_fix_never_rewinds_far() {
        let r = load().expect("model");
        let (mut s, sim) = sim_session(&r);
        s.rate = Some(16000);
        let said = "the quick brown fox jumps over the lazy dog and keeps running far away";
        s.update(said, Instant::now(), true);
        // The accuracy pass hears the first word differently: too far back to fix.
        let mut heard = words(said);
        heard[0] = "a".into();
        let segment = s.segment;
        s.apply(Fix { segment, upto: None, words: Some(heard.clone()) });
        assert_eq!(sim.borrow().screen, sim.borrow().live, "a fix far back must be left alone");
        // It also hears the last word differently: that one is fixed.
        *heard.last_mut().unwrap() = "today".into();
        s.apply(Fix { segment, upto: None, words: Some(heard) });
        assert!(sim.borrow().screen.ends_with("far today"), "{}", sim.borrow().screen);
        assert!(sim.borrow().longest_rewind <= MAX_REWIND);
    }

    #[test]
    #[ignore]
    fn partial_fix_needs_two_agreeing_reads() {
        let r = load().expect("model");
        let (mut s, sim) = sim_session(&r);
        s.rate = Some(16000);
        s.update("on the test recordings the biggest fix", Instant::now(), true);
        let segment = s.segment;
        let partial = |text: &str| Fix { segment, upto: Some(7), words: Some(words(text)) };
        // Each re-read hears it differently: nothing changes.
        s.apply(partial("unattached recordings the biggest fix rewound"));
        s.apply(partial("all the test recordings the biggest fix rewound"));
        assert_eq!(sim.borrow().screen, "On the test recordings the biggest fix");
        // Two in a row agree: that is applied (the newest typed word is left alone).
        s.apply(partial("all the test recordings the biggest fix rewound"));
        assert_eq!(sim.borrow().screen, "All the test recordings the biggest fix");
    }

    /// Replays every bundled test recording through the real typing logic, in 30 ms chunks with
    /// simulated time, running the accuracy pass in step (partial and final passes) on a simulated
    /// text box. The live-typed text must match the live model's final transcript exactly
    /// (nothing doubled or dropped), every fix must apply cleanly, and no fix may rewind further
    /// than MAX_REWIND.
    /// `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_model_on_test_wavs() {
        let r = load().expect("model");
        let mut refiner = Refiner::new();
        let mut run = |job: Option<Job>, s: &mut Session| {
            if let Some(job) = job {
                let words = refiner.transcribe(job.rate, &job.audio, job.upto.is_some()).map(|t| words_of(&t));
                s.apply(Fix { segment: job.segment, upto: job.upto, words });
            }
        };
        let wavs: Vec<_> = std::fs::read_dir(models_dir().unwrap())
            .unwrap()
            .flatten()
            .flat_map(|m| std::fs::read_dir(m.path().join("test_wavs")).into_iter().flatten().flatten())
            .filter(|f| f.path().extension().is_some_and(|e| e == "wav"))
            .collect();
        let mut mismatches = 0;
        for wav in wavs {
            let w = sherpa_onnx::Wave::read(&wav.path().to_string_lossy()).expect("wav");
            let rate = w.sample_rate() as u32;
            let (mut s, sim) = sim_session(&r);
            s.rate = Some(rate);
            let mut reference = Writer { typed_any: false, cap_next: true };
            let mut expected = String::new();
            let (t0, started) = (Instant::now(), Instant::now());
            let mut fed = 0usize;
            let at = |fed: usize| t0 + Duration::from_secs_f64(fed as f64 / rate as f64);
            for c in w.samples().chunks(rate as usize * 30 / 1000) {
                s.stream.accept_waveform(rate as i32, c);
                s.add_audio(c, rate);
                drain(&r, &s.stream);
                fed += c.len();
                let endpoint = r.is_endpoint(&s.stream);
                let text = result(&r, &s.stream);
                s.update(&text, at(fed), endpoint);
                if endpoint {
                    expected += &reference.format(&words_of(&text));
                    r.reset(&s.stream);
                    let job = s.next_segment();
                    run(job, &mut s);
                } else {
                    let job = s.partial_job();
                    run(job, &mut s);
                }
            }
            finish(&s.stream, rate);
            drain(&r, &s.stream);
            let text = result(&r, &s.stream);
            s.update(&text, at(fed), true);
            expected += &reference.format(&words_of(&text));
            let job = s.next_segment();
            run(job, &mut s);

            let sim = sim.borrow();
            println!(
                "{} ({} ms, longest rewind {} chars)\n  live:      {}\n  corrected: {}",
                wav.path().display(),
                started.elapsed().as_millis(),
                sim.longest_rewind,
                sim.live,
                sim.screen
            );
            if sim.live != expected {
                println!("  MISMATCH, transcript was: {expected}");
                mismatches += 1;
            }
            assert!(sim.longest_rewind <= MAX_REWIND);
        }
        assert_eq!(mismatches, 0, "typed text differed from the transcript");
    }
}
