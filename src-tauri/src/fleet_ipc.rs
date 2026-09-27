//! Fleet IPC for Nexus Voice (NexusPrime unit 3.22).
//!
//! IN: a second launch with `--fleet <verb> <captureId>` reaches the running
//! instance through the single-instance callback in lib.rs, which hands it to
//! `handle_argv` and returns BEFORE any window call. A fleet command never
//! shows, focuses or raises a window (NexusPrime RULES 3). Verbs:
//!   start <captureId>  start recording, as the toggle hotkey does from Idle/Error
//!   stop  <captureId>  stop recording (or stop-after-start while Starting)
//!   state <captureId>  push the current state now
//!
//! OUT: every recording-state change, every saved transcript, and a heartbeat
//! are POSTed to the voice service's guarded `/utterance` route as
//! `{source, instance, seq, captureId, state, text?}` with the machine-local
//! token header. `seq` is strictly increasing per process and `instance`
//! changes on every launch, so the service can refuse a reordered push and
//! tell a restart from a replay. One worker sends pushes in seq order.
//!
//! Where to send, and the token, are resolved from data only -- never a
//! literal port (NexusPrime RULES 66):
//!   endpoint: NEXUS_VOICE_URL, NEXUS_VOICE_PORT, %APPDATA%\NexusVoice\config.json
//!             "port", then the fleet registry's nexus-voice servicePort.
//!   token:    NEXUS_LOCAL_TOKEN, NEXUS_LOCAL_TOKEN_PATH, then
//!             %LOCALAPPDATA%\NexusPrime\local-machine-token.
//! With no endpoint, pushes are skipped and that is logged once.

use crate::commands::audio::{start_recording, stop_recording, RecorderState};
use crate::{get_recording_state, update_recording_state, AppState, RecordingState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Listener, Manager};

const LOCAL_TOKEN_HEADER: &str = "x-nexus-local";
const HEARTBEAT: Duration = Duration::from_millis(1000);
const PUSH_TIMEOUT: Duration = Duration::from_millis(1500);
const COLD_ARGV_DELAY: Duration = Duration::from_millis(2000);

static SEQ: AtomicU64 = AtomicU64::new(0);
static CAPTURE: Mutex<Option<String>> = Mutex::new(None);
static INSTANCE: OnceLock<String> = OnceLock::new();
static SENDER: OnceLock<Mutex<tokio::sync::mpsc::UnboundedSender<serde_json::Value>>> =
    OnceLock::new();
static WARNED_NO_ENDPOINT: AtomicBool = AtomicBool::new(false);

fn instance_id() -> &'static str {
    INSTANCE.get_or_init(|| {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("{}-{}", std::process::id(), ms)
    })
}

/// A capture id the voice service will accept: 1-128 of [A-Za-z0-9._:-].
fn valid_capture_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// True when argv carries a fleet command. lib.rs checks this first.
pub fn is_fleet_argv(argv: &[String]) -> bool {
    argv.iter().any(|a| a == "--fleet")
}

/// Handle one `--fleet` argv. Never touches a window.
pub fn handle_argv(app: &AppHandle, argv: &[String]) {
    let Some(i) = argv.iter().position(|a| a == "--fleet") else {
        return;
    };
    let verb = argv.get(i + 1).map(String::as_str).unwrap_or("");
    let capture = argv.get(i + 2).map(String::as_str).unwrap_or("");
    if !valid_capture_id(capture) {
        log::warn!(
            "fleet_ipc: refused --fleet {} with an invalid capture id",
            verb
        );
        return;
    }
    if let Ok(mut c) = CAPTURE.lock() {
        *c = Some(capture.to_string());
    }
    let state = get_recording_state(app);
    match verb {
        "start" => {
            if matches!(state, RecordingState::Idle | RecordingState::Error) {
                log::info!("fleet_ipc: start {} from {:?}", capture, state);
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let recorder_state = app_handle.state::<RecorderState>();
                    if let Err(e) = start_recording(app_handle.clone(), recorder_state).await {
                        log::error!("fleet_ipc: start failed: {}", e);
                        update_recording_state(&app_handle, RecordingState::Error, Some(e));
                    }
                });
            } else {
                log::info!("fleet_ipc: start {} ignored in {:?}", capture, state);
            }
        }
        "stop" => match state {
            RecordingState::Recording => {
                log::info!("fleet_ipc: stop {}", capture);
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let recorder_state = app_handle.state::<RecorderState>();
                    if let Err(e) = stop_recording(app_handle.clone(), recorder_state).await {
                        log::error!("fleet_ipc: stop failed: {}", e);
                    }
                });
            }
            RecordingState::Starting => {
                log::info!("fleet_ipc: stop {} requested while starting", capture);
                app.state::<AppState>()
                    .pending_stop_after_start
                    .store(true, Ordering::SeqCst);
            }
            other => log::info!("fleet_ipc: stop {} ignored in {:?}", capture, other),
        },
        "state" => {}
        other => {
            log::warn!("fleet_ipc: unknown verb {:?}", other);
            return;
        }
    }
    push_state(app, None);
}

/// Start the push worker, the event listeners and the heartbeat. Call once from setup.
pub fn install(app: &AppHandle) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    let _ = SENDER.set(Mutex::new(tx));

    // One worker, in seq order: a push is awaited before the next is sent.
    tauri::async_runtime::spawn(async move {
        let client = match reqwest::Client::builder().timeout(PUSH_TIMEOUT).build() {
            Ok(c) => c,
            Err(e) => {
                log::error!("fleet_ipc: no HTTP client: {}", e);
                return;
            }
        };
        while let Some(body) = rx.recv().await {
            let Some(url) = endpoint() else {
                if !WARNED_NO_ENDPOINT.swap(true, Ordering::SeqCst) {
                    log::warn!("fleet_ipc: voice service endpoint undeclared (NEXUS_VOICE_URL / NEXUS_VOICE_PORT / NexusVoice config port / registry servicePort); pushes skipped");
                }
                continue;
            };
            let mut req = client.post(&url).json(&body);
            if let Some(tok) = local_token() {
                req = req.header(LOCAL_TOKEN_HEADER, tok);
            }
            match req.send().await {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => log::warn!("fleet_ipc: push refused: HTTP {}", r.status()),
                Err(e) => log::debug!("fleet_ipc: push failed: {}", e),
            }
        }
    });

    let a = app.clone();
    app.listen_any("recording-state-changed", move |_event| {
        push_state(&a, None)
    });
    let a = app.clone();
    app.listen_any("transcription-added", move |event| {
        let text = serde_json::from_str::<serde_json::Value>(event.payload())
            .ok()
            .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_string));
        if let Some(text) = text {
            push_state(&a, Some(text));
        }
    });

    let a = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(HEARTBEAT);
        push_state(&a, None);
    });

    // A cold launch that carried --fleet (VoiceTypr was not running yet): run
    // it once the rest of setup has finished, not in the middle of it.
    let argv: Vec<String> = std::env::args().collect();
    if is_fleet_argv(&argv) {
        let a = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(COLD_ARGV_DELAY);
            handle_argv(&a, &argv);
        });
    }
}

/// Queue one push. seq is taken under the sender lock, so queue order is seq order.
fn push_state(app: &AppHandle, text: Option<String>) {
    let Some(lock) = SENDER.get() else { return };
    let Ok(tx) = lock.lock() else { return };
    let state = format!("{:?}", get_recording_state(app));
    let capture = CAPTURE.lock().ok().and_then(|c| c.clone());
    let seq = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let mut body = serde_json::json!({
        "source": "voicetypr",
        "instance": instance_id(),
        "seq": seq,
        "captureId": capture,
        "state": state,
    });
    if let Some(t) = text {
        body["text"] = serde_json::Value::String(t);
    }
    let _ = tx.send(body);
}

fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("AppData").join("Local"))
        })
}

fn endpoint() -> Option<String> {
    if let Ok(u) = std::env::var("NEXUS_VOICE_URL") {
        let u = u.trim().trim_end_matches('/');
        if !u.is_empty() {
            return Some(format!("{}/utterance", u));
        }
    }
    let valid = |p: u64| (1..=65535).contains(&p);
    let mut port: Option<u64> = std::env::var("NEXUS_VOICE_PORT")
        .ok()
        .and_then(|p| p.trim().parse().ok())
        .filter(|p| valid(*p));
    if port.is_none() {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let p = PathBuf::from(appdata)
                .join("NexusVoice")
                .join("config.json");
            port = read_json(&p)
                .and_then(|v| v.get("port").and_then(|x| x.as_u64()))
                .filter(|p| valid(*p));
        }
    }
    if port.is_none() {
        if let Some(local) = local_app_data() {
            let p = local
                .join("NexusPrime")
                .join("registry")
                .join("projects.json");
            port = read_json(&p)
                .and_then(|doc| {
                    let projects = doc.get("projects").cloned().unwrap_or(doc);
                    projects
                        .get("nexus-voice")
                        .and_then(|r| r.get("servicePort"))
                        .and_then(|x| x.as_u64())
                })
                .filter(|p| valid(*p));
        }
    }
    port.map(|p| format!("http://127.0.0.1:{}/utterance", p))
}

fn local_token() -> Option<String> {
    if let Ok(t) = std::env::var("NEXUS_LOCAL_TOKEN") {
        if !t.is_empty() {
            return Some(t);
        }
    }
    let path = std::env::var_os("NEXUS_LOCAL_TOKEN_PATH")
        .map(PathBuf::from)
        .or_else(|| local_app_data().map(|l| l.join("NexusPrime").join("local-machine-token")))?;
    let t = std::fs::read_to_string(path).ok()?;
    let t = t.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn read_json(p: &PathBuf) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_argv_is_detected_anywhere() {
        let argv = vec![
            "voicetypr.exe".to_string(),
            "--fleet".to_string(),
            "start".to_string(),
            "cap-1".to_string(),
        ];
        assert!(is_fleet_argv(&argv));
        assert!(!is_fleet_argv(&["voicetypr.exe".to_string()]));
    }

    #[test]
    fn capture_ids_are_bounded() {
        assert!(valid_capture_id("cap-1"));
        assert!(valid_capture_id("a.b_c:d-9"));
        assert!(!valid_capture_id(""));
        assert!(!valid_capture_id("has space"));
        assert!(!valid_capture_id(&"x".repeat(129)));
    }
}
