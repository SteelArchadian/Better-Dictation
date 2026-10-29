//! Updates of Better Dictation itself, from its signed GitHub Releases (the Tauri updater; the endpoint and
//! public key are in tauri.conf.json, the signing key never leaves the release PC).
//!
//! A check runs shortly after start and every 30 minutes. A newer version adds "Update to X.Y.Z" to the tray
//! menu and a banner with its notes to Settings. Nothing installs until someone presses Update: releases
//! aren't code-signed, and antivirus rightly distrusts a program that quietly replaces itself. An update
//! never cuts off dictation: it waits until dictation stops. After it installs, the new version opens
//! Settings once to show what changed.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::menu::{Menu, MenuItem};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_updater::{Update, UpdaterExt};

const WHATS_NEW: &str = "whats-new.json";
const CHECK_EVERY: Duration = Duration::from_secs(30 * 60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(6);

static FOUND: Mutex<Option<Available>> = Mutex::new(None);
static INSTALLING: AtomicBool = AtomicBool::new(false);

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq)]
pub struct Available {
    version: String,
    notes: String,
}

impl From<&Update> for Available {
    fn from(u: &Update) -> Self {
        Available { version: u.version.clone(), notes: u.body.clone().unwrap_or_default() }
    }
}

#[derive(serde::Serialize, Clone)]
struct Progress {
    /// "waiting" (for dictation to stop), "downloading" or "installing".
    stage: &'static str,
    done: u64,
    total: u64,
}

async fn find(app: &AppHandle) -> Result<Option<Update>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    match tokio::time::timeout(CHECK_TIMEOUT, updater.check()).await {
        Ok(r) => r.map_err(|e| format!("couldn't check for updates: {e}")),
        Err(_) => Err("couldn't check for updates: no answer from GitHub".into()),
    }
}

/// Remembers a found update and shows it in the tray menu and Settings.
fn announce(app: &AppHandle, menu: &Menu<Wry>, item: &MenuItem<Wry>, found: Available) {
    let mut slot = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref() == Some(&found) {
        return;
    }
    if slot.is_none() {
        let _ = menu.insert(item, 0);
    }
    let _ = item.set_text(format!("Update to {}…", found.version));
    crate::logger::log(&format!("update available: {}", found.version));
    let _ = app.emit_to("settings", "update-available", found.clone());
    *slot = Some(found);
}

/// Checks now and every 30 minutes. Being offline only means no update is shown.
pub fn start(app: AppHandle, menu: Menu<Wry>, item: MenuItem<Wry>) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            match find(&app).await {
                Ok(Some(u)) => announce(&app, &menu, &item, Available::from(&u)),
                Ok(None) => {}
                Err(e) => crate::logger::log(&e),
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

#[tauri::command]
pub fn update_info() -> Option<Available> {
    FOUND.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The "Check for updates" button: None when up to date.
#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<Option<Available>, String> {
    Ok(find(&app).await?.map(|u| {
        let a = Available::from(&u);
        *FOUND.lock().unwrap_or_else(|e| e.into_inner()) = Some(a.clone());
        a
    }))
}

/// Downloads the newer release, checks its signature and installs it; the app restarts as the new version.
#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("an update is already installing".into());
    }
    let result = install(&app).await;
    INSTALLING.store(false, Ordering::SeqCst);
    result
}

async fn install(app: &AppHandle) -> Result<(), String> {
    let progress = |stage, done, total| {
        let _ = app.emit_to("settings", "update-progress", Progress { stage, done, total });
    };
    if crate::hook::LISTENING.load(Ordering::SeqCst) {
        progress("waiting", 0, 0);
        while crate::hook::LISTENING.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    let u = find(app).await?.ok_or("Better Dictation is up to date")?;
    let mut done = 0u64;
    // The download is checked against tauri.conf.json's public key before anything runs.
    let bytes = u
        .download(
            |n, total| {
                done += n as u64;
                progress("downloading", done, total.unwrap_or(0));
            },
            || {},
        )
        .await
        .map_err(|e| format!("couldn't download the update: {e}"))?;
    progress("installing", 0, 0);
    let note = whats_new_path(app);
    if let Some(path) = &note {
        let _ = std::fs::write(path, serde_json::to_vec(&Available::from(&u)).unwrap_or_default());
    }
    crate::logger::log(&format!("installing update {}", u.version));
    // On Windows this starts the installer quietly (it starts the new version when done) and ends this process.
    u.install(bytes).map_err(|e| {
        if let Some(path) = &note {
            let _ = std::fs::remove_file(path);
        }
        format!("couldn't install the update: {e}")
    })?;
    app.restart();
}

fn whats_new_path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(WHATS_NEW))
}

/// Just updated: Settings opens once to say what changed.
pub fn just_updated(app: &AppHandle) -> bool {
    whats_new_path(app).is_some_and(|p| p.is_file())
}

/// What this version changed, once, right after an update installed it.
#[tauri::command]
pub fn whats_new(app: AppHandle) -> Option<Available> {
    let path = whats_new_path(&app)?;
    let note: Available = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    let _ = std::fs::remove_file(&path);
    (note.version == app.package_info().version.to_string()).then_some(note)
}
