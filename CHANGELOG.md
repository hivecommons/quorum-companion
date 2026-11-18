# Changelog

## 0.2.0

- Betty's listening chime: when the hive's replies say `tone`, the companion plays the same soft two-note chime as quorum (`src/tone.ts`) on this computer's default output, never into the call. A new setting, "Play Betty's listening chime" (on by default), turns it off. Needs a hive with Quorum v0.6.0 or later (hive PRs hivecommons/hive#19116 and #19121 on v5) ([#12](https://github.com/hivecommons/quorum-companion/issues/12)).

## 0.1.1

First release with Windows and Linux assets.

- Windows (x64): unsigned `.msi` and portable `.zip`.
- Linux (x86_64, PipeWire with WirePlumber): `.deb` and AppImage.
- A single `SHA256SUMS` covers every attached file.
- EXPERIMENTAL: the Windows and Linux builds have not yet been verified on a real call. Expect problems and use macOS for real meetings. To help verify, follow [docs/verify-windows.md](docs/verify-windows.md) or [docs/verify-linux.md](docs/verify-linux.md).
- macOS is unchanged from 0.1.0.

## 0.1.0

First release: macOS (Apple silicon) call detection, consent prompt, capture and streaming to the hive.
