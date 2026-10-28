mod asr;
mod audio;
mod hook;
mod logger;
mod numbers;
mod overlay;
mod refine;
mod settings;
mod typing;

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};

use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder, Wry};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

use settings::{Settings, Shortcut};

/// Everything that changes what the app is doing goes through one controller thread.
pub enum Event {
    Toggle,
    /// Carries what stopped it (key, esc, click) for the log.
    Stop(String),
    Captured(Shortcut),
    CaptureCancelled,
    MicrophoneChanged,
}

struct Shared {
    settings: Mutex<Settings>,
    path: PathBuf,
    tx: Sender<Event>,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut Settings)) -> Settings {
        let mut s = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut s);
        if let Err(e) = settings::save(&self.path, &s) {
            logger::log(&format!("saving settings failed: {e}"));
        }
        s.clone()
    }

    fn get(&self) -> Settings {
        self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[derive(Serialize, Clone)]
struct SettingsView {
    enabled: bool,
    shortcut: String,
    is_win_h: bool,
    microphone: Option<String>,
    microphones: Vec<String>,
    accuracy: String,
}

fn view(s: &Settings) -> SettingsView {
    SettingsView {
        enabled: s.enabled,
        shortcut: s.shortcut.label(),
        is_win_h: s.shortcut == Shortcut::default(),
        microphone: s.microphone.clone(),
        microphones: audio::list_microphones(),
        accuracy: s.accuracy.clone(),
    }
}

#[tauri::command]
fn set_accuracy(state: State<Shared>, accuracy: String) {
    refine::MODE.store(settings::accuracy_mode(&accuracy), Ordering::SeqCst);
    state.update(|s| s.accuracy = accuracy);
}

#[tauri::command]
fn get_settings(state: State<Shared>) -> SettingsView {
    view(&state.get())
}

#[tauri::command]
fn set_enabled(state: State<Shared>, enabled: bool) {
    state.update(|s| s.enabled = enabled);
    hook::ENABLED.store(enabled, Ordering::SeqCst);
}

#[tauri::command]
fn set_microphone(state: State<Shared>, microphone: Option<String>) {
    state.update(|s| s.microphone = microphone);
    let _ = state.tx.send(Event::MicrophoneChanged);
}

#[tauri::command]
fn begin_capture() {
    hook::CAPTURING.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn cancel_capture() {
    hook::CAPTURING.store(false, Ordering::SeqCst);
}

#[tauri::command]
fn reset_shortcut(state: State<Shared>) -> SettingsView {
    let s = state.update(|s| s.shortcut = Shortcut::default());
    hook::set_shortcut(s.shortcut);
    view(&s)
}

fn open_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("Better Dictation Settings")
        .inner_size(480.0, 400.0)
        .resizable(false)
        .center()
        .build();
    if let Err(e) = built {
        logger::log(&format!("settings window failed: {e}"));
    }
}

fn controller(app: AppHandle, rx: Receiver<Event>, toggle_item: MenuItem<Wry>) {
    let asr = asr::start();
    let audio = audio::Audio::new(app.clone(), asr.clone());
    let shared = || app.state::<Shared>();
    let mut listening = false;

    // `clean`: stopped by Esc or the shortcut, which are swallowed, so the sentence can be finished.
    let set_listening = |on: bool, clean: bool| {
        hook::LISTENING.store(on, Ordering::SeqCst);
        hook::watch_mouse(on);
        let _ = toggle_item.set_text(if on { "Stop dictation" } else { "Start dictation" });
        if on {
            let _ = asr.send(asr::Msg::Begin);
            audio.start(shared().get().microphone);
            overlay::show(&app);
        } else {
            audio.stop();
            let _ = asr.send(asr::Msg::End { clean });
            overlay::hide(&app);
        }
        logger::log(if on { "listening" } else { "stopped" });
    };

    for ev in rx {
        match ev {
            Event::Toggle => {
                listening = !listening;
                set_listening(listening, true);
            }
            Event::Stop(why) if listening => {
                logger::log(&format!("stop: {why}"));
                listening = false;
                set_listening(false, why == "esc");
            }
            Event::Stop(_) => {}
            Event::MicrophoneChanged if listening => audio.start(shared().get().microphone),
            Event::MicrophoneChanged => {}
            Event::Captured(sc) => {
                let s = shared().update(|s| s.shortcut = sc);
                hook::set_shortcut(sc);
                let _ = app.emit_to("settings", "settings-changed", view(&s));
            }
            Event::CaptureCancelled => {
                let _ = app.emit_to("settings", "settings-changed", view(&shared().get()));
            }
        }
    }
}

/// A second copy would install a second hook and fight the first over the shortcut.
fn already_running() -> bool {
    unsafe {
        let handle = CreateMutexW(None, true, w!("Local\\BetterDictation.SingleInstance"));
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        // The handle is kept for the life of the process on purpose.
        std::mem::forget(handle);
        exists
    }
}

pub fn run() {
    if already_running() {
        return;
    }

    let app = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_settings,
            set_enabled,
            set_microphone,
            set_accuracy,
            begin_capture,
            cancel_capture,
            reset_shortcut
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            logger::init(&app.path().app_log_dir()?);
            let path = settings::path(&app.path().app_config_dir()?);
            let s = settings::load(&path);
            logger::log(&format!("starting; shortcut {}", s.shortcut.label()));

            hook::ENABLED.store(s.enabled, Ordering::SeqCst);
            refine::MODE.store(settings::accuracy_mode(&s.accuracy), Ordering::SeqCst);
            hook::set_shortcut(s.shortcut);
            let (tx, rx) = mpsc::channel();
            hook::start(tx.clone());
            app.manage(Shared { settings: Mutex::new(s), path, tx: tx.clone() });

            overlay::create(&handle)?;

            let toggle = MenuItem::with_id(app, "toggle", "Start dictation", true, None::<&str>)?;
            let settings_item = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Exit", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&toggle, &settings_item, &sep, &quit])?;
            let menu_tx = tx.clone();
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().cloned().ok_or("missing app icon")?)
                .tooltip("Better Dictation")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, e| match e.id().as_ref() {
                    "toggle" => {
                        let _ = menu_tx.send(Event::Toggle);
                    }
                    "settings" => open_settings(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, e| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                        open_settings(tray.app_handle());
                    }
                })
                .build(app)?;

            std::thread::Builder::new()
                .name("controller".into())
                .spawn(move || controller(handle, rx, toggle))?;
            // `--toggle` starts listening right away (used for testing without a real keypress).
            if std::env::args().any(|a| a == "--toggle") {
                std::thread::spawn(move || {
                    // Give the model time to load so startup timing matches a real Win+H.
                    std::thread::sleep(std::time::Duration::from_secs(4));
                    let _ = tx.send(Event::Toggle);
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Better Dictation");

    app.run(|_, event| {
        // Closing the settings window must not quit the tray app; only Exit does (code Some).
        if let RunEvent::ExitRequested { code: None, api, .. } = event {
            api.prevent_exit();
        }
    });
}
