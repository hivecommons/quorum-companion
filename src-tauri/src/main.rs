//! Quorum Companion: lets hive's Betty listen to a call through the person's own meeting app.
//!
//! Flow (macOS and Windows): the capture helper's `detect` mode reports a meeting app starting to use the
//! microphone; the window comes forward with the hive's consent notice; only after the person
//! confirms the participants were told does the companion start a companion meeting on the hive
//! and run the helper's `capture` mode, streaming the app's audio and the microphone as one-second
//! PCM chunks. Betty's replies are polled into the window, and when a poll says `tone` the window
//! plays her listening chime (tone.rs) on this computer. Stopping (or the hive ending the
//! meeting, e.g. "betty leave") stops the helper. Nothing is captured without that confirmation.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod hive;
mod settings;
mod tone;

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use audio::{Channel, Chunker};
use hive::{Hive, HiveError, StartRequest};

const HELPER: &str = "quorum-capture";
/// What to check when the helper stops on its own.
#[cfg(target_os = "macos")]
const CAPTURE_STOPPED: &str =
    "the capture stopped (check Screen & System Audio Recording in System Settings)";
#[cfg(windows)]
const CAPTURE_STOPPED: &str =
    "the capture stopped (the meeting app quit, or Windows refused process loopback)";
#[cfg(not(any(target_os = "macos", windows)))]
const CAPTURE_STOPPED: &str = "the capture stopped";
/// Chunks waiting to be sent; at one second each this is two minutes of audio per channel.
const SEND_QUEUE: usize = 240;
const SEND_RETRIES: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_millis(750);
const REPLIES_POLL: Duration = Duration::from_secs(2);

struct Session {
    meeting_id: String,
    app: String,
    stop: Arc<AtomicBool>,
    helper: Child,
    /// Held open while capturing; the helper exits when it closes.
    helper_stdin: Option<ChildStdin>,
}

#[derive(Default)]
struct AppState {
    session: Mutex<Option<Session>>,
    detector: Mutex<Option<(Child, Option<ChildStdin>)>>,
}

#[derive(Clone, Serialize)]
struct SettingsView {
    hive_url: String,
    host_name: String,
    offer_on_call: bool,
    listening_chime: bool,
    has_token: bool,
}

/// The listening chime for the window to play (tone.rs): mono signed 16-bit samples.
#[derive(Clone, Serialize)]
struct ToneView {
    sample_rate: u32,
    samples: Vec<i16>,
}

#[derive(Clone, Serialize)]
struct SessionView {
    meeting_id: String,
    app: String,
}

fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_config_dir().map_err(|e| e.to_string())
}

fn helper_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(exe.with_file_name(format!("{HELPER}{}", std::env::consts::EXE_SUFFIX)))
}

/// The capture helper as a command; on Windows it runs without a console window.
fn helper_command() -> Result<Command, String> {
    #[allow(unused_mut)]
    let mut command = Command::new(helper_path()?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(command)
}

fn spawn_helper(args: &[&str]) -> Result<Child, String> {
    helper_command()?
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot start the capture helper: {e}"))
}

fn client(app: &AppHandle) -> Result<Hive, String> {
    let s = settings::load(&config_dir(app)?);
    if s.hive_url.is_empty() {
        return Err("set your hive URL first".into());
    }
    let token = settings::token(&s.hive_url).unwrap_or_default();
    Hive::new(&s.hive_url, &token).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Result<SettingsView, String> {
    let s = settings::load(&config_dir(&app)?);
    let has_token = !s.hive_url.is_empty() && settings::token(&s.hive_url).is_some();
    Ok(SettingsView {
        hive_url: s.hive_url,
        host_name: s.host_name,
        offer_on_call: s.offer_on_call,
        listening_chime: s.listening_chime,
        has_token,
    })
}

#[tauri::command]
fn save_settings(
    app: AppHandle,
    hive_url: String,
    host_name: String,
    offer_on_call: bool,
    listening_chime: bool,
    token: Option<String>,
) -> Result<SettingsView, String> {
    let url = hive::validate_base(&hive_url).map_err(|e| e.to_string())?;
    let dir = config_dir(&app)?;
    settings::save(
        &dir,
        &settings::Settings {
            hive_url: url.clone(),
            host_name: host_name.trim().to_string(),
            offer_on_call,
            listening_chime,
        },
    )?;
    if let Some(t) = token {
        settings::set_token(&url, &t)?;
    }
    get_settings(app)
}

#[tauri::command]
fn listening_tone() -> ToneView {
    ToneView {
        sample_rate: tone::TONE_RATE_HZ,
        samples: tone::render(),
    }
}

#[tauri::command]
async fn companion_info(app: AppHandle) -> Result<hive::Info, String> {
    tauri::async_runtime::spawn_blocking(move || client(&app)?.info().map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn list_apps() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let out = helper_command()?
            .arg("list")
            .output()
            .map_err(|e| e.to_string())?;
        serde_json::from_slice(&out.stdout).map_err(|e| format!("capture helper: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn current_session(state: State<'_, AppState>) -> Option<SessionView> {
    state.session.lock().ok()?.as_ref().map(|s| SessionView {
        meeting_id: s.meeting_id.clone(),
        app: s.app.clone(),
    })
}

/// Starts a companion meeting and the capture. `consent_attested` is the person's confirmation
/// that the participants were told; the hive refuses a start without it.
#[tauri::command]
async fn start_capture(
    app: AppHandle,
    bundle_id: String,
    app_name: String,
    title: String,
    consent_version: String,
    consent_attested: bool,
) -> Result<SessionView, String> {
    if !consent_attested {
        return Err("confirm that everyone in the call was told and agreed".into());
    }
    {
        let state = app.state::<AppState>();
        if state.session.lock().map_err(|e| e.to_string())?.is_some() {
            return Err("Betty is already listening to a call".into());
        }
    }
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        start_blocking(handle, bundle_id, app_name, title, consent_version)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn start_blocking(
    app: AppHandle,
    bundle_id: String,
    app_name: String,
    title: String,
    consent_version: String,
) -> Result<SessionView, String> {
    let hive = Arc::new(client(&app)?);
    let host = settings::load(&config_dir(&app)?).host_name;
    let meeting = hive
        .start(&StartRequest {
            title: &title,
            app: &app_name,
            host_name: &host,
            consent_version: &consent_version,
            consent_attested: true,
        })
        .map_err(|e| e.to_string())?;
    let mut helper = match spawn_helper(&["capture", &bundle_id]) {
        Ok(h) => h,
        Err(e) => {
            let _ = hive.end(&meeting.id);
            return Err(e);
        }
    };
    let stdout = helper.stdout.take().ok_or("capture helper has no stdout")?;
    let stdin = helper.stdin.take();
    let stop = Arc::new(AtomicBool::new(false));

    let (tx, rx) = mpsc::sync_channel::<(Channel, Vec<u8>)>(SEND_QUEUE);
    spawn_reader(app.clone(), stdout, tx, stop.clone());
    spawn_sender(app.clone(), hive.clone(), meeting.id.clone(), rx);
    spawn_replies(app.clone(), hive, meeting.id.clone(), stop.clone());

    let view = SessionView {
        meeting_id: meeting.id.clone(),
        app: app_name.clone(),
    };
    let state = app.state::<AppState>();
    *state.session.lock().map_err(|e| e.to_string())? = Some(Session {
        meeting_id: meeting.id,
        app: app_name,
        stop,
        helper,
        helper_stdin: stdin,
    });
    let _ = app.emit("session-started", view.clone());
    Ok(view)
}

/// Reads helper frames into per-channel chunks for the sender; at the end of the stream (the
/// helper stopped) the remaining audio is flushed.
fn spawn_reader(
    app: AppHandle,
    stdout: std::process::ChildStdout,
    tx: SyncSender<(Channel, Vec<u8>)>,
    stop: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut chunkers: HashMap<Channel, Chunker> = HashMap::new();
        loop {
            match audio::read_frame(&mut reader) {
                Ok(Some((channel, pcm))) => {
                    for chunk in chunkers.entry(channel).or_default().push(&pcm) {
                        if tx.try_send((channel, chunk)).is_err() {
                            let _ = app.emit(
                                "capture-warning",
                                "the hive is not keeping up; some audio was dropped",
                            );
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = app.emit("capture-error", format!("capture helper: {e}"));
                    break;
                }
            }
        }
        for (channel, mut c) in chunkers {
            if let Some(rest) = c.flush() {
                let _ = tx.send((channel, rest));
            }
        }
        if !stop.load(Ordering::SeqCst) {
            let _ = app.emit("capture-error", CAPTURE_STOPPED);
        }
    });
}

/// Posts chunks in order per channel. A gap answer moves the channel to the seq the hive wants
/// (the audio in between is lost, not resent); network errors are retried a few times.
fn spawn_sender(
    app: AppHandle,
    hive: Arc<Hive>,
    meeting_id: String,
    rx: Receiver<(Channel, Vec<u8>)>,
) {
    thread::spawn(move || {
        let mut seqs: HashMap<Channel, u64> = HashMap::new();
        for (channel, pcm) in rx {
            let seq = seqs.entry(channel).or_insert(0);
            let mut attempt = 0;
            loop {
                match hive.audio(&meeting_id, channel.as_str(), *seq, pcm.clone()) {
                    Ok(()) => {
                        *seq += 1;
                        break;
                    }
                    Err(HiveError::Gap { expected }) if expected != *seq => {
                        *seq = expected;
                    }
                    Err(HiveError::Network(_)) if attempt < SEND_RETRIES => {
                        attempt += 1;
                        thread::sleep(RETRY_DELAY);
                    }
                    Err(e) => {
                        let _ = app.emit("capture-warning", e.to_string());
                        *seq += 1;
                        break;
                    }
                }
            }
        }
    });
}

/// Polls Betty's lines into the window until the session stops or the hive ends the meeting,
/// and asks the window to play the listening chime when a poll says `tone` (unless the person
/// turned it off, or a chime is still playing).
fn spawn_replies(app: AppHandle, hive: Arc<Hive>, meeting_id: String, stop: Arc<AtomicBool>) {
    thread::spawn(move || {
        let mut after = 0;
        let mut chime = tone::ToneGate::default();
        while !stop.load(Ordering::SeqCst) {
            match hive.replies(&meeting_id, after) {
                Ok(r) => {
                    after = r.next_seq;
                    if r.tone {
                        let enabled = config_dir(&app)
                            .map(|d| settings::load(&d).listening_chime)
                            .unwrap_or(true);
                        chime.offer(enabled, Instant::now(), || {
                            let _ = app.emit("listening-tone", ());
                        });
                    }
                    if !r.lines.is_empty() {
                        let _ = app.emit("replies", r.lines);
                    }
                    if r.state != "live" {
                        let _ = app.emit("meeting-ended", r.state);
                        stop_session(&app, false);
                        return;
                    }
                }
                Err(e) => {
                    let _ = app.emit("capture-warning", e.to_string());
                }
            }
            thread::sleep(REPLIES_POLL);
        }
    });
}

/// Ends the capture; with `tell_hive` the companion meeting is ended on the hive too.
fn stop_session(app: &AppHandle, tell_hive: bool) {
    let state = app.state::<AppState>();
    let session = match state.session.lock() {
        Ok(mut s) => s.take(),
        Err(_) => None,
    };
    let Some(mut session) = session else { return };
    session.stop.store(true, Ordering::SeqCst);
    drop(session.helper_stdin.take());
    let _ = session.helper.kill();
    let _ = session.helper.wait();
    if tell_hive {
        if let Ok(hive) = client(app) {
            let _ = hive.end(&session.meeting_id);
        }
    }
    let _ = app.emit("session-stopped", session.meeting_id);
}

#[tauri::command]
async fn stop_capture(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || stop_session(&app, true))
        .await
        .map_err(|e| e.to_string())
}

/// Runs the helper's `detect` mode and brings the window forward when a call starts.
fn start_detector(app: &AppHandle) {
    let mut child = match spawn_helper(&["detect"]) {
        Ok(c) => c,
        Err(e) => {
            let _ = app.emit("capture-error", e);
            return;
        }
    };
    let Some(stdout) = child.stdout.take() else {
        return;
    };
    let stdin = child.stdin.take();
    if let Ok(mut d) = app.state::<AppState>().detector.lock() {
        *d = Some((child, stdin));
    }
    let app = app.clone();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let kind = event
                .get("event")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let offer = config_dir(&app)
                .map(|d| settings::load(&d).offer_on_call)
                .unwrap_or(true);
            if kind == "call_started" && offer {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.unminimize();
                    let _ = w.set_focus();
                }
            }
            if forwards(&kind, offer) {
                let _ = app.emit(&kind.replace('_', "-"), event);
            }
        }
    });
}

/// Whether a detector event reaches the UI. With "Offer to listen when a call
/// starts" off, `call_started` is dropped so the UI never asks the hive or shows
/// the consent prompt for a detected call; every other event still flows.
fn forwards(kind: &str, offer_on_call: bool) -> bool {
    kind != "call_started" || offer_on_call
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            companion_info,
            listening_tone,
            list_apps,
            current_session,
            start_capture,
            stop_capture
        ])
        .setup(|app| {
            start_detector(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the companion")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                stop_session(app, true);
                if let Ok(mut d) = app.state::<AppState>().detector.lock() {
                    if let Some((mut child, stdin)) = d.take() {
                        drop(stdin);
                        let _ = child.kill();
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::forwards;

    #[test]
    fn call_started_is_dropped_when_offer_is_off() {
        assert!(!forwards("call_started", false));
    }

    #[test]
    fn call_started_is_forwarded_when_offer_is_on() {
        assert!(forwards("call_started", true));
    }

    #[test]
    fn call_ended_is_always_forwarded() {
        assert!(forwards("call_ended", false));
        assert!(forwards("call_ended", true));
    }
}
