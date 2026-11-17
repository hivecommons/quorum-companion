#!/usr/bin/env bash
# Builds the Windows app: the Rust capture helper (capture/windows), the icons, then the Tauri
# bundle (.msi). Runs in Git Bash on a Windows runner. Unsigned: there is no code-signing
# certificate, so SmartScreen warns on first launch (see README).
set -euo pipefail

TARGET="${TARGET:-x86_64-pc-windows-msvc}"
case "$TARGET" in
  x86_64-pc-windows-msvc | aarch64-pc-windows-msvc) ;;
  *) echo "unsupported TARGET $TARGET" >&2; exit 2 ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
mkdir -p src-tauri/binaries
cargo build --release --manifest-path capture/windows/Cargo.toml --target "$TARGET"
cp "capture/windows/target/$TARGET/release/quorum-capture.exe" \
  "src-tauri/binaries/quorum-capture-$TARGET.exe"
npx tauri icon app-icon.png --output src-tauri/icons
npx tauri build --target "$TARGET" --bundles msi
