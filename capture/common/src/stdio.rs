//! The helper's side of the stdout/stdin contract with the Tauri host: frames and JSON lines
//! written under the stdout lock, a stderr log line, and exit when the parent closes stdin.
//! Shared by the Linux and Windows helpers so the two copies cannot drift.

use std::io::{self, Read, Write};
use std::sync::OnceLock;

/// What to run instead of a bare `process::exit` when the helper must stop (for example to stop
/// child recorders first). The helper then exits from inside the hook.
static EXIT_HOOK: OnceLock<fn(i32)> = OnceLock::new();

/// Registers the exit hook. Only the first call has an effect.
pub fn set_exit_hook(hook: fn(i32)) {
    let _ = EXIT_HOOK.set(hook);
}

fn exit(code: i32) -> ! {
    if let Some(hook) = EXIT_HOOK.get() {
        hook(code);
    }
    std::process::exit(code)
}

pub fn log(message: &str) {
    eprintln!("quorum-capture: {message}");
}

/// Writes one frame or line under the stdout lock, so the call and microphone threads never
/// interleave.
pub fn write_stdout(bytes: &[u8]) {
    let mut out = io::stdout().lock();
    if out.write_all(bytes).and_then(|()| out.flush()).is_err() {
        drop(out);
        // The app is gone; never keep capturing alone.
        exit(0);
    }
}

pub fn write_frame(channel: u8, pcm: &[u8]) {
    if !pcm.is_empty() {
        write_stdout(&crate::frame(channel, pcm));
    }
}

pub fn emit_json(value: &serde_json::Value) {
    let mut line = value.to_string();
    line.push('\n');
    write_stdout(line.as_bytes());
}

/// Exits when the parent closes stdin (or dies), so the helper never keeps capturing alone.
pub fn exit_when_stdin_closes() {
    std::thread::spawn(|| {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 256];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => exit(0),
                Ok(_) => {}
            }
        }
    });
}
