# Quorum Companion design (v1, macOS and Windows)

Recommendation and comparison: [hivecommons/hive#18596](https://github.com/hivecommons/hive/issues/18596). Reference product pattern: Plaud Desktop (detects a call, offers to capture it on the host's machine, no bot).

## Goals and limits

- Betty takes part in a call through the person's own meeting app. Nothing joins the meeting; no platform's bot detection is involved or bypassed.
- Consent first: no capture before the person confirms the participants were told (the hive refuses a start without the current notice version and that confirmation, and records who confirmed, when, and which version). A red "Betty is listening" indicator stays on for the whole capture.
- Audio leaves the machine only while a capture the person started runs, only to their own hive, over TLS (plain HTTP is accepted only for a hive on `localhost`).
- v1: listen and answer in the companion window. Spoken replies through a virtual microphone are a later step (they need a virtual audio device and an explicit "Betty is speaking" attribution in the call).

## Parts

```
capture/macos/main.swift     quorum-capture helper (Swift): detect | list | capture <bundle-id>
capture/windows/             quorum-capture.exe helper (Rust, WASAPI): detect | list | capture <exe>
src-tauri/src/main.rs        Tauri app: detection, consent prompt, session, threads
src-tauri/src/audio.rs       helper frame protocol, 1 s chunker
src-tauri/src/hive.rs        hive companion API client
src-tauri/src/settings.rs    hive URL (file) and companion token (keychain)
ui/                          the window (plain HTML/JS, no remote content)
```

One codebase (Tauri) with per-OS capture helpers that speak the same stdout protocol:

| OS | Call audio | Microphone | Call detection | Status |
|---|---|---|---|---|
| macOS 14.2+ | ScreenCaptureKit, filter = the meeting app | AVAudioEngine input | Core Audio process objects: `kAudioProcessPropertyIsRunningInput` for the app's bundle id | v1 |
| Windows 10 2004+ | WASAPI process loopback (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`, include the process tree) | WASAPI capture, default communications microphone | active audio sessions on the capture endpoints, mapped to the app through the parent process chain | v1 prototype |
| Linux | PipeWire: capture the app's output node | PipeWire source | PipeWire node metadata (`application.process.binary`) | after |

### Frame protocol (helper stdout)

`[1 byte channel: 0 = call, 1 = mic][4 bytes little-endian length][16 kHz mono s16le PCM]`. Logs go to stderr. The helper exits when its stdin closes, so it never outlives the app.

### Windows capture module

`capture/windows` is a small Rust binary (the `windows` crate), built for `x86_64-pc-windows-msvc` and bundled as the Tauri sidecar `quorum-capture.exe`. It speaks the same CLI and frame protocol as the macOS helper; the app id is the meeting app's executable name in lower case (`zoom.exe`, `ms-teams.exe`, `chrome.exe`).

- **Detection**: every 2 s it lists the active audio sessions of every active capture endpoint (`IAudioSessionManager2`), maps each session's process to a meeting app by walking the parent process chain (a Chrome renderer belongs to `chrome.exe`, a WebView2 child to `ms-teams.exe`), and reports `call_started` / `call_ended` with the same two-poll grace as macOS.
- **Call channel**: `ActivateAudioInterfaceAsync` on `VAD\Process_Loopback` with `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` and `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`. The target is the top-level process of the executable (its parent is a different executable); with several top-level instances, the one whose tree holds an active microphone session wins. The stream is 48 kHz stereo s16 (the virtual device converts), event-driven.
- **Mic channel**: the default communications capture endpoint in its mix format (usually float32), event-driven.
- **Conversion**: each packet is downmixed to mono, resampled to 16 kHz with an area-averaging (box) filter that keeps state across packets, and written as s16le frames. This logic is platform-independent and unit-tested on both CI runners.
- **Consent, indicator, notice, stop and token** are the app's (shared with macOS): the token is stored with the `keyring` crate's `windows-native` backend, i.e. a generic credential in Windows Credential Manager.
- **Known limits**: a meeting app launched by a browser and still its child (rare) is part of the browser's tree; process loopback needs Windows 10 2004+; the build is unsigned (SmartScreen asks once).

### Detection

On macOS, `quorum-capture detect` polls Core Audio's process list every 2 s and reports `call_started` when a known meeting app (or its helper process, e.g. Chrome's renderer) is using the microphone, `call_ended` after two polls without. The window comes forward with the prompt; the person can also pick a running app and press **Ask now**.

### Streaming to the hive

Hive API (owner or read-write; a companion meeting is usable only by its starter or an owner):

1. `GET /api/companion/info`: the consent notice, its version, sample rate, chunk limit.
2. `POST /api/companion/meetings {title, app, host_name, consent_version, consent_attested: true}`.
3. `POST /api/companion/meetings/{id}/audio?channel=call|mic&seq=N`: one-second chunks, per channel in order from 0. A 409 with `expected_seq` moves the channel to that seq (the audio in between is lost, never resent out of order). Network errors are retried three times.
4. `GET /api/companion/meetings/{id}/replies?after=N` every 2 s: Betty's lines and the meeting state; a state other than `live` (for example after "betty leave") stops the capture.
5. `POST /api/companion/meetings/{id}/end` on Stop or when the app quits.

Hive relays the PCM to Quorum's `companion` platform, which cuts each channel at silences, transcribes it with the shared Whisper service and feeds Betty's normal live pipeline. Speaker attribution: the `mic` channel is the person, the `call` channel is "Call" (the meeting app mixes everyone else).

Auth: a per-user companion token minted in the hive dashboard, sent as `X-Hive-Companion-Token`, admitted on `/api/companion/*` only, acting as the minting user at their current role (revoking the user or the token stops it at once). On hosted hives the hub lets `/api/companion/` through to the hive, which does the check. The token lives in the macOS keychain, one per hive URL.

### Echo

Without headphones the microphone also hears the speakers, so the call can be transcribed twice. v1 recommends headphones. A later version can use voice processing on the microphone input (AVAudioEngine `setVoiceProcessingEnabled`) or drop `mic` segments that duplicate a `call` segment.

## Signing and permissions

There is no Apple Developer ID, so builds are **ad-hoc signed** (`codesign -s -`, Tauri `signingIdentity: "-"`) with the hardened runtime and the `com.apple.security.device.audio-input` entitlement, and are not notarized. Gatekeeper therefore blocks the first launch until the person clicks **Open Anyway** in System Settings, Privacy & Security. Homebrew 5 removed `--no-quarantine`, so the cask cannot skip that step.

macOS ties privacy grants (Screen & System Audio Recording, Microphone) to the app's **designated requirement**. An ad-hoc signature has no stable designated requirement (it is the code hash), so every new build looks like a new app and macOS may ask again after an update.

Mitigation:

- The bundle id (`dev.hivecommons.quorum-companion`) and the install path (`/Applications/Quorum Companion.app`) never change.
- Releases can be signed with **one self-signed code-signing certificate** kept as repository secrets (`MACOS_SIGNING_P12` base64, `MACOS_SIGNING_P12_PASSWORD`, `MACOS_SIGNING_IDENTITY` = the certificate's common name). A certificate gives a designated requirement of `identifier "dev.hivecommons.quorum-companion" and certificate leaf = H"<hash>"`, which stays the same across builds, so grants survive updates. Gatekeeper still asks for Open Anyway once per install (the certificate is not Apple-issued). The release workflow uses the certificate when the secrets exist and falls back to ad-hoc otherwise.
- To create it (owner, once): Keychain Access, Certificate Assistant, Create a Certificate, type **Code Signing**, name it (for example "Hive Commons Companion"), export it as .p12 with a password, then add the three secrets to the repository. Never commit the .p12.
- If a grant is stale, `tccutil reset ScreenCapture dev.hivecommons.quorum-companion` (and `Microphone`) clears it.

## Distribution

- GitHub Release per tag `vX.Y.Z`: `QuorumCompanion-X.Y.Z-macos-arm64.dmg`, `.zip`, `QuorumCompanion-X.Y.Z-windows-x64.msi`, `.zip` (portable: the app and `quorum-capture.exe`) and `SHA256SUMS`. Windows builds are unsigned.
- Homebrew cask `quorum-companion` in `hivecommons/homebrew-hive`, pointing at the release zip with its sha256, `depends_on macos: ">= :sonoma"`, with caveats describing Open Anyway and the two permissions.

## Not in v1

Spoken replies (virtual microphone), Linux capture, Windows on Arm, Intel Macs, automatic echo suppression, auto-start at login, a menu-bar-only mode.
