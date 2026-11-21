//! WASAPI on Windows: process snapshot, microphone session detection, process loopback for the
//! meeting app's audio and the default microphone.
//!
//! The call channel uses `ActivateAudioInterfaceAsync` on the process loopback virtual device
//! with `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` and
//! `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`, so only the meeting app (and its helper
//! processes, e.g. a browser's audio service or Teams' WebView2) is captured, never other apps,
//! no virtual audio driver involved. Windows 10 version 2004 (build 19041) or later.

use std::collections::BTreeSet;
use std::ffi::c_void;
use std::mem::{size_of, ManuallyDrop};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::json;
use windows::core::imp::{BLOB, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::core::PROPVARIANT;
use windows::core::{implement, Error, IUnknown, Interface, Result, HRESULT, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE};
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, ActivateAudioInterfaceAsync, AudioSessionStateActive,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
    IAudioSessionControl2, IAudioSessionEnumerator, IAudioSessionManager2, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    DEVICE_STATE_ACTIVE, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;

use crate::apps::{self, Detector, Process};
use crate::pcm::{self, Format, Resampler, SampleKind, CHANNEL_CALL, CHANNEL_MIC, TARGET_RATE};
use crate::log;
use quorum_capture_common::stdio::{emit_json, write_frame};

const POLL: Duration = Duration::from_secs(2);
const MISSES_TO_END: u32 = 2;
/// The shared-mode buffer WASAPI keeps for us, in 100 ns units (200 ms).
const BUFFER_HNS: i64 = 2_000_000;
/// How long a capture thread waits for WASAPI's "data ready" event before checking anyway.
const WAIT_MS: u32 = 200;
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(10);
/// `AUDCLNT_BUFFERFLAGS_SILENT`: the packet is silence and its data must not be read.
const BUFFERFLAGS_SILENT: u32 = 0x2;
/// What the call channel asks process loopback for; the virtual device converts to it.
const LOOPBACK_FORMAT: Format = Format {
    kind: SampleKind::I16,
    channels: 2,
    rate: 48_000,
};

fn com_init() {
    // Every thread that touches WASAPI joins the multithreaded apartment; "already initialized"
    // is fine.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|c| *c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

/// A snapshot of the running processes (pid, parent pid, executable name).
fn processes() -> Vec<Process> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                out.push(Process {
                    pid: entry.th32ProcessID,
                    parent: entry.th32ParentProcessID,
                    exe: wide_to_string(&entry.szExeFile),
                });
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    out
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

/// Processes with an active audio session on any active capture device, i.e. using a microphone.
fn microphone_pids() -> Vec<u32> {
    match capture_session_pids() {
        Ok(pids) => pids.into_iter().collect(),
        Err(e) => {
            log(&format!("cannot list microphone sessions: {e}"));
            Vec::new()
        }
    }
}

fn capture_session_pids() -> Result<BTreeSet<u32>> {
    let mut pids = BTreeSet::new();
    unsafe {
        let devices = enumerator()?.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        for i in 0..devices.GetCount()? {
            // A device that fails (unplugged meanwhile) is skipped, not fatal.
            if let Ok(found) = device_session_pids(&devices.Item(i)?) {
                pids.extend(found);
            }
        }
    }
    Ok(pids)
}

fn device_session_pids(device: &IMMDevice) -> Result<Vec<u32>> {
    let mut pids = Vec::new();
    unsafe {
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let sessions = manager.GetSessionEnumerator()?;
        for index in 0..sessions.GetCount()? {
            if let Some(pid) = active_session_pid(&sessions, index) {
                pids.push(pid);
            }
        }
    }
    Ok(pids)
}

fn active_session_pid(sessions: &IAudioSessionEnumerator, index: i32) -> Option<u32> {
    unsafe {
        let session: IAudioSessionControl2 = sessions.GetSession(index).ok()?.cast().ok()?;
        if session.GetState().ok()? != AudioSessionStateActive {
            return None;
        }
        session.GetProcessId().ok()
    }
}

pub fn run_detect() -> ! {
    com_init();
    let mut detector = Detector::new(MISSES_TO_END);
    loop {
        let now = apps::apps_in_call(&microphone_pids(), &processes());
        for event in detector.poll(&now) {
            let (kind, exe, app) = match event {
                apps::CallEvent::Started { exe, app } => ("call_started", exe, app),
                apps::CallEvent::Ended { exe, app } => ("call_ended", exe, app),
            };
            emit_json(&json!({"event": kind, "bundle_id": exe, "app": app}));
        }
        thread::sleep(POLL);
    }
}

pub fn run_list() -> ! {
    com_init();
    let procs = processes();
    let in_call = apps::apps_in_call(&microphone_pids(), &procs);
    let list: Vec<serde_json::Value> = apps::running_apps(&procs)
        .into_iter()
        .map(|m| apps::app_json(m, in_call.contains_key(m.exe)))
        .collect();
    emit_json(&serde_json::Value::Array(list));
    std::process::exit(0);
}

/// Signals when `ActivateAudioInterfaceAsync` has finished.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(Arc<(Mutex<bool>, Condvar)>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated {
    fn ActivateCompleted(
        &self,
        _operation: Option<&IActivateAudioInterfaceAsyncOperation>,
    ) -> Result<()> {
        let (done, signal) = &*self.0;
        if let Ok(mut done) = done.lock() {
            *done = true;
        }
        signal.notify_all();
        Ok(())
    }
}

/// An audio client on the process loopback virtual device for `pid` and its process tree.
fn process_loopback_client(pid: u32) -> Result<IAudioClient> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // A VT_BLOB PROPVARIANT pointing at `params`; ManuallyDrop because the blob is ours, not
    // CoTaskMem memory PropVariantClear could free.
    let prop = ManuallyDrop::new(unsafe {
        PROPVARIANT::from_raw(windows::core::imp::PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: PROPVARIANT_0_0 {
                    vt: VT_BLOB.0,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB {
                            cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: &mut params as *mut AUDIOCLIENT_ACTIVATION_PARAMS as *mut u8,
                        },
                    },
                },
            },
        })
    });
    let state = Arc::new((Mutex::new(false), Condvar::new()));
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(state.clone()).into();
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*prop as *const PROPVARIANT),
            &handler,
        )?
    };
    {
        let (done, signal) = &*state;
        let guard = done.lock().map_err(|_| Error::from(E_FAIL))?;
        let (guard, _) = signal
            .wait_timeout_while(guard, ACTIVATE_TIMEOUT, |done| !*done)
            .map_err(|_| Error::from(E_FAIL))?;
        if !*guard {
            log("process loopback activation timed out");
            return Err(Error::from(E_FAIL));
        }
    }
    let mut activated = HRESULT(0);
    let mut client: Option<IUnknown> = None;
    unsafe { operation.GetActivateResult(&mut activated, &mut client)? };
    activated.ok()?;
    client.ok_or_else(|| Error::from(E_FAIL))?.cast()
}

fn wave_format(format: Format) -> WAVEFORMATEX {
    let block = format.frame_bytes() as u16;
    WAVEFORMATEX {
        wFormatTag: 1, // WAVE_FORMAT_PCM
        nChannels: format.channels,
        nSamplesPerSec: format.rate,
        nAvgBytesPerSec: format.rate * u32::from(block),
        nBlockAlign: block,
        wBitsPerSample: (format.kind.bytes() * 8) as u16,
        cbSize: 0,
    }
}

/// Reads a `WAVEFORMATEX` (or `WAVEFORMATEXTENSIBLE`) WASAPI handed out.
///
/// # Safety
///
/// `ptr` must point to a valid wave format; an extensible one must have its full size.
unsafe fn read_format(ptr: *const WAVEFORMATEX) -> Option<Format> {
    let f = std::ptr::read_unaligned(ptr);
    let tag = f.wFormatTag;
    let bits = f.wBitsPerSample;
    let channels = f.nChannels;
    let rate = f.nSamplesPerSec;
    let extra = f.cbSize;
    // WAVEFORMATEXTENSIBLE: 18 bytes of WAVEFORMATEX, Samples (2), dwChannelMask (4), then the
    // SubFormat GUID whose Data1 tells PCM (1) from float (3).
    let sub_format = if tag == 0xFFFE && extra >= 22 {
        Some(std::ptr::read_unaligned(
            ptr.cast::<u8>().add(24).cast::<u32>(),
        ))
    } else {
        None
    };
    let kind = SampleKind::from_wave(tag, bits, sub_format)?;
    if channels == 0 || rate == 0 {
        return None;
    }
    Some(Format {
        kind,
        channels,
        rate,
    })
}

/// The default communications microphone, in its own mix format.
fn microphone_client() -> Result<(IAudioClient, Format)> {
    unsafe {
        let device = enumerator()?.GetDefaultAudioEndpoint(eCapture, eCommunications)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let mix = client.GetMixFormat()?;
        let format = read_format(mix);
        let init = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            BUFFER_HNS,
            0,
            mix,
            None,
        );
        CoTaskMemFree(Some(mix as *const c_void));
        init?;
        let format = format.ok_or_else(|| Error::from(E_FAIL))?;
        Ok((client, format))
    }
}

/// The meeting app's audio through process loopback, in `LOOPBACK_FORMAT`.
fn call_client(pid: u32) -> Result<IAudioClient> {
    let client = process_loopback_client(pid)?;
    let format = wave_format(LOOPBACK_FORMAT);
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            BUFFER_HNS,
            0,
            &format,
            None,
        )?;
    }
    Ok(client)
}

/// Pulls packets from a started client and writes them as 16 kHz mono frames until an error.
fn pump(client: &IAudioClient, format: Format, channel: u8) -> Result<()> {
    unsafe {
        let capture: IAudioCaptureClient = client.GetService()?;
        let event: HANDLE = CreateEventW(None, false, false, PCWSTR::null())?;
        client.SetEventHandle(event)?;
        client.Start()?;
        let mut resampler = Resampler::new(format.rate, TARGET_RATE);
        let frame_bytes = format.frame_bytes();
        loop {
            let _ = WaitForSingleObject(event, WAIT_MS);
            while capture.GetNextPacketSize()? > 0 {
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let mono = if flags & BUFFERFLAGS_SILENT != 0 || data.is_null() {
                    vec![0.0; frames as usize]
                } else {
                    let bytes = std::slice::from_raw_parts(data, frames as usize * frame_bytes);
                    pcm::to_mono(bytes, format)
                };
                capture.ReleaseBuffer(frames)?;
                write_frame(channel, &pcm::to_s16le(&resampler.push(&mono)));
            }
        }
    }
}

pub fn run_capture(exe: &str) -> ! {
    com_init();
    let procs = processes();
    let Some(root) = apps::capture_root(exe, &procs, &microphone_pids()) else {
        log(&format!("cannot capture {exe}: it is not running"));
        std::process::exit(2);
    };
    thread::spawn(|| {
        com_init();
        match microphone_client() {
            Ok((client, format)) => {
                if let Err(e) = pump(&client, format, CHANNEL_MIC) {
                    log(&format!("microphone stopped: {e}"));
                }
            }
            Err(e) => {
                log(&format!(
                    "microphone unavailable, capturing the call audio only: {e}"
                ));
                log("check Settings, Privacy & security, Microphone, desktop app access");
            }
        }
    });
    let client = match call_client(root) {
        Ok(c) => c,
        Err(e) => {
            log(&format!("cannot capture {exe} (pid {root}): {e}"));
            log("process loopback needs Windows 10 version 2004 or later");
            std::process::exit(2);
        }
    };
    eprintln!(
        "{}",
        json!({"event": "capturing", "bundle_id": exe, "pid": root})
    );
    if let Err(e) = pump(&client, LOOPBACK_FORMAT, CHANNEL_CALL) {
        log(&format!("call audio stopped: {e}"));
    }
    std::process::exit(3);
}
