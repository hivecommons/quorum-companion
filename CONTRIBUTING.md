# Contributing to Quorum Companion

Every commit must carry a Developer Certificate of Origin sign-off (`git commit -s`), certifying you have the right to submit the change under the project license. See https://developercertificate.org/.

Open an issue before large changes. Keep pull requests focused, with tests for new behaviour. PR titles start with an emoji (`✨` feature, `🐛` fix, `📖` docs, `🌱` chore).

- The companion streams to hive's companion API; its contract lives in hive `src/docs/meetings.md` ("Desktop companion") and quorum `docs/contract.md` ("Companion audio"). A change to it changes those docs first.
- Consent is not optional: nothing may be captured or sent before the person confirmed the participants were told, and the "Betty is listening" indicator stays visible for the whole capture.
- Capture modules are per OS (`capture/macos`, `capture/windows`, `capture/linux`) and speak the same frame protocol on stdout (see `src-tauri/src/audio.rs`). They never touch the network. Code the Rust helpers share (PCM framing, the resampler, the stdout/stdin plumbing) lives in the platform-neutral `capture/common` crate (`quorum-capture-common`).
- No platform's bot detection is ever bypassed: the companion only reads audio from the person's own meeting app on their own computer.

Ask in the [Hive Commons Discord](https://hivecommons.dev/discord) if in doubt.

## Building locally

The build scripts are the same ones CI runs, so a green local build matches CI. Every OS needs Node 22 and a stable Rust toolchain (`rustup`); the Tauri CLI is installed by `npm ci` from `package.json`. Run `npm ci` once at the repository root first.

- macOS (Apple silicon, 14.2 or later): Xcode command line tools for `swiftc`, then `scripts/build-macos.sh`.
- Windows (x64): Visual Studio Build Tools with the C++ workload and the WebView2 runtime, then `scripts/build-windows.sh` from Git Bash.
- Linux (x86_64): the system libraries below, then `scripts/build-linux.sh`.

  ```
  sudo apt-get install -y --no-install-recommends \
    build-essential pkg-config file patchelf libfuse2 xdg-utils \
    libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev
  ```

Each script builds the per-OS capture helper into `src-tauri/binaries/`, generates the icons from `app-icon.png`, then runs `tauri build`. The Rust capture helpers have unit tests: run `cargo test` in `capture/common`, `capture/windows` or `capture/linux`. Testing a helper does not run the shared crate's tests, so run them in `capture/common` whenever you change it.

## Cargo.lock

Each Rust crate root (`src-tauri`, `capture/linux`, `capture/windows`) should commit its own `Cargo.lock` (not yet committed; tracked in #26) so CI and release builds use pinned, reviewable dependency versions. Never add `Cargo.lock` to `.gitignore`. When you change a `Cargo.toml`, run `cargo generate-lockfile` or `cargo update -p <crate>` in that crate and commit the lockfile diff in the same pull request. Once the lockfiles are committed, CI passes `--locked` to cargo so a drifted lockfile fails fast.
