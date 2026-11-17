#!/usr/bin/env bash
# Builds the Linux app: the Rust capture helper (capture/linux), the icons, then the Tauri
# bundles (.deb and AppImage). Needs the WebKitGTK 4.1 and D-Bus development packages (see the
# linux job in .github/workflows/ci-linux.yml). Unsigned.
set -euo pipefail

TARGET="${TARGET:-x86_64-unknown-linux-gnu}"
case "$TARGET" in
  x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu) ;;
  *) echo "unsupported TARGET $TARGET" >&2; exit 2 ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
mkdir -p src-tauri/binaries
cargo build --release --manifest-path capture/linux/Cargo.toml --target "$TARGET"
cp "capture/linux/target/$TARGET/release/quorum-capture" \
  "src-tauri/binaries/quorum-capture-$TARGET"
npx tauri icon app-icon.png --output src-tauri/icons
npx tauri build --target "$TARGET" --bundles deb,appimage
