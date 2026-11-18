# Quorum Companion

A small desktop app that lets [hive](https://github.com/hivecommons/hive)'s meeting assistant, Betty, take part in a call **through your own meeting app**, after you confirm the participants were told. Nothing joins the meeting: the companion captures your meeting app's audio and your microphone on your own computer and streams them to your own hive, which transcribes them, takes notes, researches and answers. Betty's replies appear in the companion window.

It is how Betty works on Zoom without a Zoom app or bot (Zoom does not allow automated web-client bots, and the companion does not try to get around that), and it works the same for Teams, Google Meet in a browser, Webex and Slack huddles.

Design and comparison: [hivecommons/hive#18596](https://github.com/hivecommons/hive/issues/18596). Streaming contract: hive [docs/meetings.md, "Desktop companion"](https://github.com/hivecommons/hive/blob/v5/src/docs/meetings.md) and quorum [docs/contract.md, "Companion audio"](https://github.com/hivecommons/quorum/blob/main/docs/contract.md).

Status: **macOS v1 prototype** (Apple silicon, macOS 14.2 or later), **Windows prototype** (x64, Windows 10 version 2004 or later, WASAPI process loopback) and **Linux prototype** (x86_64, PipeWire). Windows and Linux are experimental: they build in CI but have not been verified on a real call. To help verify, follow [Verify on Windows](docs/verify-windows.md) or [Verify on Linux](docs/verify-linux.md).

## How it works

1. The companion notices a meeting app start using the microphone (macOS: Core Audio process objects; Windows: WASAPI audio sessions; Zoom, Teams, Webex, Slack, Chrome, Edge, Arc, Safari, and on Windows also Firefox and Brave).
2. It asks **"Let Betty listen to this call?"** and shows your hive's consent notice. You tick "I told everyone in this call that Betty is listening, and they agreed" and press **Let Betty listen**. Without that, nothing is captured. **Copy notice** puts the notice on the clipboard to paste into the meeting chat.
3. While it captures, the window shows a red **Betty is listening** indicator. The meeting app's audio (macOS: ScreenCaptureKit; Windows: WASAPI process loopback of the app and its helper processes; no virtual audio driver either way) and your microphone are sent to your hive as 16 kHz mono PCM, one second at a time, over HTTPS.
4. Betty's lines (the consent notice first, then her answers) appear in the window. **Stop**, or saying "betty leave" in the call, ends it.
5. When Betty heard her name and is about to answer, the companion plays her **listening chime** on your computer's default output: the same soft two-note chime she plays on other platforms (quorum `src/tone.ts`: G5 then C6, 100 ms each, peak at 18% of full scale with a smooth attack and release). Only you hear it; it is never sent into the call (the companion captures only the meeting app's own audio). It is dropped while a chime is still playing and arrives with the next replies poll (at most about two seconds later). Turn it off in **Settings** with "Play Betty's listening chime". It needs a hive and quorum that send the chime to companion meetings (quorum `say.tone`, hive's replies route `tone` field); with an older hive the companion simply plays nothing.

What leaves your computer: only the audio of a capture you started, only to the hive URL you configured. The companion token is kept in the macOS keychain or in Windows Credential Manager (a generic credential under `dev.hivecommons.quorum-companion`). Settings (hive URL, your display name, the listening chime) are in `~/Library/Application Support/dev.hivecommons.quorum-companion/` on macOS and `%APPDATA%\dev.hivecommons.quorum-companion\` on Windows.

## Install (macOS)

The app is **ad-hoc signed, not notarized** (the project has no Apple Developer ID), so macOS asks once before it opens it.

- Homebrew: `brew install --cask hivecommons/hive/quorum-companion`
- Or download the `.dmg` (or `.zip`) from [Releases](https://github.com/hivecommons/quorum-companion/releases) and drag **Quorum Companion** to Applications.

First launch:

1. Open the app. macOS says it cannot verify the developer. Open **System Settings, Privacy & Security**, scroll to the message about Quorum Companion and click **Open Anyway**, then confirm.
2. When you first let Betty listen, macOS asks for **Microphone** and **Screen & System Audio Recording** (the companion captures only audio; ScreenCaptureKit is the API macOS provides for one app's audio). Allow both; you may need to reopen the app after granting Screen & System Audio Recording.
3. In the dashboard of your hive, create a companion token, then paste the hive URL and the token into the companion's **Settings**.

**After an update** macOS may ask for Screen & System Audio Recording again: an ad-hoc signature changes with every build, and macOS ties the permission to the signature. Releases signed with the project's stable self-signed certificate keep the permission; see [docs/design.md](docs/design.md#signing-and-permissions). If the permission looks granted but capture fails, run `tccutil reset ScreenCapture dev.hivecommons.quorum-companion` and allow it again.

## Install (Windows)

The app is **unsigned** (the project has no code-signing certificate), so Windows SmartScreen warns once.

- Download `QuorumCompanion-X.Y.Z-windows-x64.msi` from [Releases](https://github.com/hivecommons/quorum-companion/releases) and run it, or unzip the portable `.zip` anywhere and run `Quorum Companion.exe` (keep `quorum-capture.exe` next to it). It needs the WebView2 runtime, which Windows 10 and 11 normally have.
- SmartScreen says "Windows protected your PC": click **More info**, then **Run anyway**.
- Microphone: **Settings, Privacy & security, Microphone** must allow **Let desktop apps access your microphone**. Capturing the meeting app's audio needs no permission (process loopback is limited to that app's process tree), but it needs Windows 10 version 2004 (build 19041) or later.
- In the dashboard of your hive, create a companion token, then paste the hive URL and the token into the companion's **Settings**.

On Windows the app id the companion shows and passes to the capture helper is the meeting app's executable (`zoom.exe`, `ms-teams.exe`, `chrome.exe`, ...). The helper captures the top-level process of that executable with its whole process tree, so a browser's audio service and new Teams' WebView2 processes are included. If you run two separate instances of a browser, the one using the microphone is chosen.

## Building

CI builds every pull request on macOS (`scripts/build-macos.sh`: the Swift capture helper with `swiftc`, the icons, then `tauri build`) and uploads the `.dmg`, and on Windows (`scripts/build-windows.sh`: the Rust capture helper in `capture/windows`, the icons, then `tauri build --bundles msi`) and uploads the `.msi`. The Windows helper's pure logic (resampler, PCM conversion, process-tree selection, call detection) is unit-tested on both runners with `cargo test` in `capture/windows`. Tags `v*` publish one release (`release.yml`) with the macOS `.dmg`/`.zip`, the Windows `.msi`/`.zip`, the Linux `.deb`/AppImage and a single `SHA256SUMS`; a failed Windows or Linux job does not block the macOS assets. To exercise the Windows and Linux packaging without publishing anything, run the release workflow by hand (Actions, release, Run workflow) or open a PR touching it: only the Windows and Linux jobs run and their files are kept as workflow artifacts.

## Linux (prototype)

Status: **experimental prototype, not yet verified on a real call** (see [Verify on Linux](docs/verify-linux.md); x86_64; any desktop running PipeWire with WirePlumber, e.g. Ubuntu 22.10+, Debian 12+, Fedora 34+). The window, the consent prompt, the "Betty is listening" indicator, **Copy notice** and **Stop** are the same as on macOS.

- Install the `.deb` from [Releases](https://github.com/hivecommons/quorum-companion/releases) (`sudo apt install ./QuorumCompanion-X.Y.Z-linux-x64.deb`, which pulls in `pipewire-bin` and `pulseaudio-utils`), or make the AppImage executable and run it (it needs `pw-dump`, `pw-record` and, for the fallback, `parec` and `pactl` from the system).
- Call audio: the companion records **only the meeting app's own output stream** (PipeWire per-node capture: `pw-record --target <the app's stream node>`), never the whole speaker mix and with no virtual device. Where `pw-record` cannot do that (an older PipeWire such as 0.3.48, or a session manager that refuses the link) it records the same stream through pipewire-pulse (`parec --monitor-stream=<the app's sink-input>`). The microphone is the default source.
- Call detection: a meeting app (Zoom, Teams for Linux, Slack, Webex, Chrome, Chromium, Edge, Brave, Firefox, Vivaldi) has a running PipeWire recording stream, i.e. it uses the microphone. The app id the window shows is the executable name (`zoom`, `teams-for-linux`, `chrome`, ...).
- The companion token is kept in the **Secret Service** keyring (GNOME Keyring, KWallet with its Secret Service enabled, or KeePassXC), one entry per hive URL under the service `dev.hivecommons.quorum-companion`. Without a Secret Service provider (some minimal window-manager setups), saving the token fails with an error; install and unlock one (for example `gnome-keyring`) first. Settings are in `~/.config/dev.hivecommons.quorum-companion/`.
- CI builds the `.deb` and the AppImage on Ubuntu 22.04 (`scripts/build-linux.sh`: the Rust helper in `capture/linux`, the icons, then `tauri build --bundles deb,appimage`) and unit-tests the helper's platform-independent logic (resampler, framing, app lookup, stream choice, detection). Design notes: [docs/design.md](docs/design.md#linux-capture-module).

## License

Apache-2.0. Every commit needs a DCO sign-off (`git commit -s`); see [CONTRIBUTING.md](CONTRIBUTING.md).
