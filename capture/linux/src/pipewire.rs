//! The Linux side of the helper: reads the PipeWire graph with `pw-dump`, records with
//! `pw-record` (or `parec` through pipewire-pulse when `pw-record` cannot), and writes frames.
//!
//! The helper drives the stock PipeWire and PulseAudio command line tools instead of linking
//! libpipewire: the tools are on every PipeWire desktop, they keep the helper a plain Rust
//! binary that builds anywhere, and the session manager (WirePlumber) makes the links as it
//! does for any recorder. The call channel targets the meeting app's own output stream node
//! (`--target <object.serial>`), so only that app is captured, never the whole speaker mix;
//! `node.dont-fallback` keeps the session manager from linking the recorder to the default
//! microphone if that stream goes away.

use std::io::{self, ErrorKind, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::graph::{self, Detector, StreamNode};
use crate::pcm::{self, Converter};
use crate::{apps, log, shutdown};
use quorum_capture_common::stdio::{emit_json, write_frame};

/// How often the graph is polled (detection, and the capture's choice of stream).
const POLL: Duration = Duration::from_secs(2);
/// A call ends after this many polls without the app recording (as on macOS).
const MISSES_TO_END: u32 = 2;
/// The capture exits after this many polls without the meeting app (it quit).
const MISSES_TO_QUIT: u32 = 3;
/// The rate the recorders deliver; the PipeWire graph usually runs at it.
const CAPTURE_RATE: u32 = 48_000;
const CALL_CHANNELS: u16 = 2;
const MIC_CHANNELS: u16 = 1;
/// One read from a recorder: 50 ms of 48 kHz stereo s16.
const READ_BYTES: usize = 9_600;

/// The recorders the helper started, so it can stop them before it exits.
static RECORDERS: Mutex<Vec<Child>> = Mutex::new(Vec::new());
static PW_DUMP_WARNED: AtomicBool = AtomicBool::new(false);

/// Kills every recorder (on exit).
pub fn stop_recorders() {
    if let Ok(mut children) = RECORDERS.lock() {
        for child in children.iter_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        children.clear();
    }
}

fn stop_recorder(pid: u32) {
    let child = RECORDERS.lock().ok().and_then(|mut children| {
        let i = children.iter().position(|c| c.id() == pid)?;
        Some(children.swap_remove(i))
    });
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn recorder_exited(pid: u32) -> bool {
    let Ok(mut children) = RECORDERS.lock() else {
        return true;
    };
    match children.iter_mut().find(|c| c.id() == pid) {
        Some(child) => !matches!(child.try_wait(), Ok(None)),
        None => true,
    }
}

/// The meeting app streams in the graph now; none when `pw-dump` is missing or fails.
fn stream_nodes() -> Vec<StreamNode> {
    let out = Command::new("pw-dump")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(out) if out.status.success() => {
            graph::parse_pw_dump(&String::from_utf8_lossy(&out.stdout))
        }
        _ => {
            if !PW_DUMP_WARNED.swap(true, Ordering::Relaxed) {
                log("pw-dump failed: is PipeWire running and pipewire-bin installed?");
            }
            Vec::new()
        }
    }
}

/// Names and executable paths of the running processes (from `/proc`).
fn process_names() -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return names;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let path = entry.path();
        if let Ok(comm) = std::fs::read_to_string(path.join("comm")) {
            names.push(comm.trim().to_string());
        }
        if let Ok(exe) = std::fs::read_link(path.join("exe")) {
            names.push(exe.to_string_lossy().into_owned());
        }
    }
    names
}

/// Whether the meeting app runs: a process of it, or a stream of it in the graph.
fn app_running(app_id: &str, nodes: &[StreamNode]) -> bool {
    let names = process_names();
    let running = apps::running(names.iter().map(String::as_str));
    running.contains(app_id) || graph::apps_with_streams(nodes).contains(app_id)
}

pub fn run_detect() -> ! {
    let mut detector = Detector::new(MISSES_TO_END);
    loop {
        let in_call = graph::apps_in_call(&stream_nodes());
        for event in detector.step(&in_call) {
            emit_json(&event.to_json());
        }
        thread::sleep(POLL);
    }
}

pub fn run_list() -> ! {
    let nodes = stream_nodes();
    let in_call = graph::apps_in_call(&nodes);
    let names = process_names();
    let mut running = apps::running(names.iter().map(String::as_str));
    running.extend(graph::apps_with_streams(&nodes));
    emit_json(&apps::list_json(&running, &in_call));
    shutdown(0)
}

/// Which tool records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    /// `pw-record`: PipeWire per-node capture.
    PipeWire,
    /// `parec` through pipewire-pulse: the sink-input monitor, or the default source.
    Pulse,
}

/// A running recorder and how much audio it gave.
struct Recorder {
    pid: u32,
    bytes: Arc<AtomicU64>,
    started: Instant,
}

impl Recorder {
    fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
}

/// `pw-record` writing raw s16le to stdout; `target` is the serial of the node to record, or
/// `None` for the default source.
fn pw_record(target: Option<u64>, channels: u16, node_name: &str) -> Command {
    let mut props = json!({
        "node.name": node_name,
        "node.description": "Quorum Companion capture",
        "application.name": "Quorum Companion",
    });
    let mut command = Command::new("pw-record");
    command
        .arg("--rate")
        .arg(CAPTURE_RATE.to_string())
        .arg("--channels")
        .arg(channels.to_string())
        .arg("--format")
        .arg("s16");
    if let Some(serial) = target {
        command.arg("--target").arg(serial.to_string());
        // Never record something else (the default microphone) instead of the app's stream.
        props["node.dont-fallback"] = Value::Bool(true);
        props["node.dont-reconnect"] = Value::Bool(true);
        props["node.dont-move"] = Value::Bool(true);
    }
    command.arg("-P").arg(props.to_string()).arg("-");
    command
}

/// `parec` writing raw s16le to stdout from `source`, a `--monitor-stream=` or `--device=`
/// argument.
fn parec(channels: u16, source: String) -> Command {
    let mut command = Command::new("parec");
    command
        .arg("--raw")
        .arg("--format=s16le")
        .arg(format!("--rate={CAPTURE_RATE}"))
        .arg(format!("--channels={channels}"))
        .arg("--client-name=Quorum Companion")
        .arg(source);
    command
}

/// The pipewire-pulse sink-input index of an output stream; pipewire-pulse numbers
/// sink-inputs by `object.serial`, which is the answer when `pactl` cannot tell.
fn sink_input_index(serial: u64, app_id: &str) -> u64 {
    let out = Command::new("pactl")
        .args(["--format=json", "list", "sink-inputs"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let json = out
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    graph::sink_input_index(&json, serial, app_id).unwrap_or(serial)
}

/// Starts a recorder and a thread that turns its output into frames on `channel`.
fn start_recorder(mut command: Command, channel: u8, channels: u16) -> Option<Recorder> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    // SAFETY: prctl is async-signal-safe; the closure allocates nothing. The recorder gets
    // SIGTERM if the helper dies without stopping it, so it never records on alone.
    unsafe {
        command.pre_exec(|| {
            let sigterm = libc::SIGTERM as libc::c_ulong;
            if libc::prctl(libc::PR_SET_PDEATHSIG, sigterm) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            log(&format!("cannot start {program}: {e}"));
            return None;
        }
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let pid = child.id();
    if let Ok(mut children) = RECORDERS.lock() {
        children.push(child);
    }
    let bytes = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&bytes);
    thread::spawn(move || {
        let mut converter = Converter::new(CAPTURE_RATE, channels);
        let mut buf = vec![0u8; READ_BYTES];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    counter.fetch_add(n as u64, Ordering::Relaxed);
                    write_frame(channel, &converter.push(&buf[..n]));
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
    Some(Recorder {
        pid,
        bytes,
        started: Instant::now(),
    })
}

/// Records the default source as the `mic` channel for as long as the helper runs.
fn run_mic() {
    let mut backend = Backend::PipeWire;
    loop {
        let command = match backend {
            Backend::PipeWire => pw_record(None, MIC_CHANNELS, "quorum-capture-mic"),
            Backend::Pulse => parec(MIC_CHANNELS, "--device=@DEFAULT_SOURCE@".to_string()),
        };
        let Some(recorder) = start_recorder(command, pcm::CHANNEL_MIC, MIC_CHANNELS) else {
            if backend == Backend::Pulse {
                log("microphone unavailable: capturing the call audio only");
                return;
            }
            backend = Backend::Pulse;
            continue;
        };
        loop {
            thread::sleep(POLL);
            let exited = recorder_exited(recorder.pid);
            let elapsed = recorder.started.elapsed();
            let bytes = recorder.bytes();
            let pipewire = backend == Backend::PipeWire;
            if pipewire && graph::should_fall_back(bytes, elapsed, exited, true) {
                log("pw-record gave no microphone audio; recording with parec (pipewire-pulse)");
                stop_recorder(recorder.pid);
                backend = Backend::Pulse;
                break;
            }
            if exited {
                stop_recorder(recorder.pid);
                if backend == Backend::Pulse && bytes == 0 {
                    log("microphone unavailable: capturing the call audio only");
                    return;
                }
                log("microphone recorder stopped; restarting it");
                break;
            }
        }
    }
}

/// Starts recording the app's output stream `serial` as the `call` channel.
fn start_call(backend: Backend, serial: u64, app_id: &str) -> Option<Recorder> {
    let command = match backend {
        Backend::PipeWire => pw_record(Some(serial), CALL_CHANNELS, "quorum-capture-call"),
        Backend::Pulse => {
            let index = sink_input_index(serial, app_id);
            parec(CALL_CHANNELS, format!("--monitor-stream={index}"))
        }
    };
    let recorder = start_recorder(command, pcm::CHANNEL_CALL, CALL_CHANNELS)?;
    log(&format!("recording {app_id} stream {serial} ({backend:?})"));
    Some(recorder)
}

/// Records the meeting app's output stream as the `call` channel, following the app to a new
/// stream when it opens one. Exits when the app quits.
fn run_call(app_id: &'static str) -> ! {
    let mut backend = Backend::PipeWire;
    let mut current: Option<(u64, Recorder)> = None;
    let mut announced = false;
    let mut waiting_logged = false;
    let mut misses = 0;
    loop {
        let nodes = stream_nodes();
        if app_running(app_id, &nodes) {
            misses = 0;
        } else {
            misses += 1;
            if misses >= MISSES_TO_QUIT {
                log(&format!("{app_id} quit: the capture stops"));
                shutdown(3);
            }
        }
        let target = graph::next_target(current.as_ref().map(|(s, _)| *s), &nodes, app_id);
        if let Some((serial, recorder)) = current.take() {
            let exited = recorder_exited(recorder.pid);
            let playing = nodes.iter().any(|n| n.serial == serial && n.running);
            let elapsed = recorder.started.elapsed();
            let pipewire = backend == Backend::PipeWire;
            let bytes = recorder.bytes();
            let fall_back = pipewire && graph::should_fall_back(bytes, elapsed, exited, playing);
            if fall_back || exited || target != Some(serial) {
                stop_recorder(recorder.pid);
            } else {
                current = Some((serial, recorder));
            }
            if fall_back {
                log("pw-record gave no call audio; recording with parec (pipewire-pulse)");
                backend = Backend::Pulse;
            }
        }
        match (current.is_none(), target) {
            (true, Some(serial)) => match start_call(backend, serial, app_id) {
                Some(recorder) => {
                    if !announced {
                        let event = json!({ "event": "capturing", "bundle_id": app_id });
                        eprintln!("{event}");
                        announced = true;
                    }
                    current = Some((serial, recorder));
                }
                None => backend = Backend::Pulse,
            },
            (true, None) if !waiting_logged => {
                log(&format!(
                    "waiting for {app_id} to open an audio output stream"
                ));
                waiting_logged = true;
            }
            _ => {}
        }
        thread::sleep(POLL);
    }
}

pub fn run_capture(id: &str) -> ! {
    let Some(app) = apps::by_id(id) else {
        log(&format!("unknown meeting app {id}"));
        shutdown(64)
    };
    if !app_running(app.id, &stream_nodes()) {
        log(&format!("cannot capture {id}: it is not running"));
        shutdown(2)
    }
    thread::spawn(run_mic);
    run_call(app.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn props(command: &Command) -> Value {
        let args = args(command);
        let at = args.iter().position(|a| a == "-P").expect("-P present");
        serde_json::from_str(&args[at + 1]).expect("-P is JSON")
    }

    #[test]
    fn call_recorder_targets_the_app_stream_and_never_falls_back() {
        let command = pw_record(Some(42), CALL_CHANNELS, "quorum-call");
        assert_eq!(command.get_program(), "pw-record");
        let args = args(&command);
        let at = args.iter().position(|a| a == "--target").unwrap();
        assert_eq!(args[at + 1], "42");
        assert_eq!(args.last().map(String::as_str), Some("-"));
        let props = props(&command);
        // Without these the session manager could link the recorder to the default
        // microphone when the app's stream goes away, capturing the wrong audio.
        assert_eq!(props["node.dont-fallback"], true);
        assert_eq!(props["node.dont-reconnect"], true);
        assert_eq!(props["node.dont-move"], true);
        assert_eq!(props["node.name"], "quorum-call");
    }

    #[test]
    fn mic_recorder_has_no_target_and_may_follow_the_default_source() {
        let command = pw_record(None, MIC_CHANNELS, "quorum-mic");
        let args = args(&command);
        assert!(!args.iter().any(|a| a == "--target"));
        let props = props(&command);
        assert!(props.get("node.dont-fallback").is_none());
        assert_eq!(props["node.name"], "quorum-mic");
    }

    #[test]
    fn pw_record_asks_for_the_capture_format() {
        let args = args(&pw_record(None, 1, "n"));
        let pair = |flag: &str, value: &str| {
            let at = args.iter().position(|a| a == flag).unwrap();
            args[at + 1] == value
        };
        assert!(pair("--rate", &CAPTURE_RATE.to_string()));
        assert!(pair("--channels", "1"));
        assert!(pair("--format", "s16"));
    }

    #[test]
    fn parec_passes_the_source_last_in_raw_s16le() {
        let command = parec(2, "--monitor-stream=7".to_string());
        assert_eq!(command.get_program(), "parec");
        let args = args(&command);
        assert_eq!(args.first().map(String::as_str), Some("--raw"));
        assert!(args.contains(&"--format=s16le".to_string()));
        assert!(args.contains(&format!("--rate={CAPTURE_RATE}")));
        assert!(args.contains(&"--channels=2".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("--monitor-stream=7"));
    }
}
