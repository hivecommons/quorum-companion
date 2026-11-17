//! quorum-capture: the Linux capture module of the Quorum companion.
//!
//! The Tauri app runs this helper as a child process, like the macOS helper in capture/macos. It
//! never talks to the network; it only reads audio on this computer and writes to stdout.
//!
//!   quorum-capture detect          JSON lines on stdout when a meeting app starts or stops using
//!                                  the microphone (its PipeWire recording stream is running).
//!   quorum-capture list            One JSON array of the meeting apps that are running.
//!   quorum-capture capture <app>   Captures that app's output stream node (PipeWire per-node
//!                                  capture with `pw-record`, falling back to the PulseAudio
//!                                  monitor of its sink-input with `parec` on pipewire-pulse)
//!                                  and the default source as the microphone, as 16 kHz mono
//!                                  s16le PCM. Frames on stdout: 1 byte channel (0 = call,
//!                                  1 = mic), 4 bytes little-endian length, then the PCM bytes.
//!
//! The app id (`bundle_id` in the JSON) is a short executable name, e.g. `zoom` or `chrome`.
//! The helper exits when its stdin closes, so it never outlives the app; it stops its recorders
//! first, and they also get SIGTERM if the helper dies. Logs go to stderr.

// The pure modules are only wired up on Linux; elsewhere they are built for their tests.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

mod apps;
mod graph;
mod pcm;
#[cfg(target_os = "linux")]
mod pipewire;

use std::io::{self, Read, Write};

fn log(message: &str) {
    eprintln!("quorum-capture: {message}");
}

/// Stops the recorders and exits.
fn shutdown(code: i32) -> ! {
    #[cfg(target_os = "linux")]
    pipewire::stop_recorders();
    std::process::exit(code)
}

/// Writes one frame or line under the stdout lock, so the call and microphone threads never
/// interleave.
fn write_stdout(bytes: &[u8]) {
    let mut out = io::stdout().lock();
    if out.write_all(bytes).and_then(|()| out.flush()).is_err() {
        drop(out);
        // The app is gone; never keep capturing alone.
        shutdown(0);
    }
}

fn write_frame(channel: u8, pcm: &[u8]) {
    if !pcm.is_empty() {
        write_stdout(&pcm::frame(channel, pcm));
    }
}

fn emit_json(value: &serde_json::Value) {
    let mut line = value.to_string();
    line.push('\n');
    write_stdout(line.as_bytes());
}

/// Exits when the parent closes stdin (or dies), so the helper never keeps capturing alone.
fn exit_when_stdin_closes() {
    std::thread::spawn(|| {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 256];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => shutdown(0),
                Ok(_) => {}
            }
        }
    });
}

const USAGE: &str = "usage: quorum-capture detect | list | capture <app>";

#[cfg(target_os = "linux")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    match (args.get(1).map(String::as_str), args.get(2)) {
        (Some("detect"), _) => {
            exit_when_stdin_closes();
            pipewire::run_detect()
        }
        (Some("list"), _) => pipewire::run_list(),
        (Some("capture"), Some(app)) => {
            exit_when_stdin_closes();
            pipewire::run_capture(app)
        }
        _ => {
            log(USAGE);
            std::process::exit(64);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    log(USAGE);
    log("this capture module runs on Linux; macOS uses capture/macos");
    std::process::exit(69);
}
