# Quorum Companion

A small desktop app that lets [hive](https://github.com/hivecommons/hive)'s meeting assistant, Betty, take part in a call **through your own meeting app**, after you confirm the participants were told. Nothing joins the meeting: the companion captures your meeting app's audio and your microphone on your own computer and streams them to your own hive, which transcribes them, takes notes, researches and answers. Betty's replies appear in the companion window.

It is how Betty works on Zoom without a Zoom app or bot (Zoom does not allow automated web-client bots, and the companion does not try to get around that), and it works the same for Teams, Google Meet in a browser, Webex and Slack huddles.

Design and comparison: [hivecommons/hive#18596](https://github.com/hivecommons/hive/issues/18596). Streaming contract: hive [docs/meetings.md, "Desktop companion"](https://github.com/hivecommons/hive/blob/v5/src/docs/meetings.md) and quorum [docs/contract.md, "Companion audio"](https://github.com/hivecommons/quorum/blob/main/docs/contract.md).

Status: **macOS v1 prototype** (Apple silicon, macOS 14.2 or later). Windows (WASAPI per-process loopback) and Linux (PipeWire) capture modules come next, in the same app.

## How it works

1. The companion notices a meeting app start using the microphone (Core Audio process objects; Zoom, Teams, Webex, Slack, Chrome, Edge, Arc, Safari).
2. It asks **"Let Betty listen to this call?"** and shows your hive's consent notice. You tick "I told everyone in this call that Betty is listening, and they agreed" and press **Let Betty listen**. Without that, nothing is captured. **Copy notice** puts the notice on the clipboard to paste into the meeting chat.
3. While it captures, the window shows a red **Betty is listening** indicator. The meeting app's audio (ScreenCaptureKit, no virtual audio driver) and your microphone are sent to your hive as 16 kHz mono PCM, one second at a time, over HTTPS.
4. Betty's lines (the consent notice first, then her answers) appear in the window. **Stop**, or saying "betty leave" in the call, ends it.

What leaves your computer: only the audio of a capture you started, only to the hive URL you configured. The companion token is kept in the macOS keychain. Settings (hive URL, your display name) are in `~/Library/Application Support/dev.hivecommons.quorum-companion/`.

## Install (macOS)

The app is **ad-hoc signed, not notarized** (the project has no Apple Developer ID), so macOS asks once before it opens it.

- Homebrew: `brew install --cask hivecommons/hive/quorum-companion`
- Or download the `.dmg` (or `.zip`) from [Releases](https://github.com/hivecommons/quorum-companion/releases) and drag **Quorum Companion** to Applications.

First launch:

1. Open the app. macOS says it cannot verify the developer. Open **System Settings, Privacy & Security**, scroll to the message about Quorum Companion and click **Open Anyway**, then confirm.
2. When you first let Betty listen, macOS asks for **Microphone** and **Screen & System Audio Recording** (the companion captures only audio; ScreenCaptureKit is the API macOS provides for one app's audio). Allow both; you may need to reopen the app after granting Screen & System Audio Recording.
3. In the dashboard of your hive, create a companion token, then paste the hive URL and the token into the companion's **Settings**.

**After an update** macOS may ask for Screen & System Audio Recording again: an ad-hoc signature changes with every build, and macOS ties the permission to the signature. Releases signed with the project's stable self-signed certificate keep the permission; see [docs/design.md](docs/design.md#signing-and-permissions). If the permission looks granted but capture fails, run `tccutil reset ScreenCapture dev.hivecommons.quorum-companion` and allow it again.

## Building

CI builds every pull request on macOS (`scripts/build-macos.sh`: the Swift capture helper with `swiftc`, the icons, then `tauri build`) and uploads the `.dmg`. Tags `v*` publish a release.

## License

Apache-2.0. Every commit needs a DCO sign-off (`git commit -s`); see [CONTRIBUTING.md](CONTRIBUTING.md).
