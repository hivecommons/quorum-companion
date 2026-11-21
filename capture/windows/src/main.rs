//! quorum-capture: the Windows capture module of the Quorum companion.
//!
//! The Tauri app runs this helper as a child process, like the macOS helper in capture/macos. It
//! never talks to the network; it only reads audio on this computer and writes to stdout.
//!
//!   quorum-capture detect          JSON lines on stdout when a meeting app starts or stops using
//!                                  the microphone (WASAPI audio session enumeration).
//!   quorum-capture list            One JSON array of the meeting apps that are running.
//!   quorum-capture capture <exe>   Captures that app's audio, with its whole process tree
//!                                  (WASAPI process loopback, Windows 10 2004 or later), and the
//!                                  default microphone, as 16 kHz mono s16le PCM. Frames on
//!                                  stdout: 1 byte channel (0 = call, 1 = mic), 4 bytes
//!                                  little-endian length, then the PCM bytes.
//!
//! The app id (`bundle_id` in the JSON) is the meeting app's executable name, e.g. `zoom.exe`.
//! The helper exits when its stdin closes, so it never outlives the app. Logs go to stderr.

// The pure modules are only wired up on Windows; elsewhere they are built for their tests.
#![cfg_attr(not(windows), allow(dead_code))]

mod apps;
mod pcm;
#[cfg(windows)]
mod wasapi;

use quorum_capture_common::stdio::log;

const USAGE: &str = "usage: quorum-capture detect | list | capture <exe>";

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    match (args.get(1).map(String::as_str), args.get(2)) {
        (Some("detect"), _) => {
            quorum_capture_common::stdio::exit_when_stdin_closes();
            wasapi::run_detect()
        }
        (Some("list"), _) => wasapi::run_list(),
        (Some("capture"), Some(exe)) => {
            quorum_capture_common::stdio::exit_when_stdin_closes();
            wasapi::run_capture(exe)
        }
        _ => {
            log(USAGE);
            std::process::exit(64);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    log(USAGE);
    log("this capture module runs on Windows; macOS uses capture/macos");
    std::process::exit(69);
}
