#!/usr/bin/env bash
# Builds the macOS app: the Swift capture helper, the icons, then the Tauri bundle (.app + .dmg).
# Signing: ad-hoc (`codesign -s -`) unless APPLE_CERTIFICATE / APPLE_SIGNING_IDENTITY are set
# (a stable self-signed identity keeps macOS privacy grants across updates; see docs/design.md).
set -euo pipefail

TARGET="${TARGET:-aarch64-apple-darwin}"
case "$TARGET" in
  aarch64-apple-darwin) SWIFT_TARGET="arm64-apple-macos14.2" ;;
  x86_64-apple-darwin) SWIFT_TARGET="x86_64-apple-macos14.2" ;;
  *) echo "unsupported TARGET $TARGET" >&2; exit 2 ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
mkdir -p src-tauri/binaries
swiftc -O -swift-version 5 -target "$SWIFT_TARGET" \
  capture/macos/main.swift -o "src-tauri/binaries/quorum-capture-$TARGET"
npx tauri icon app-icon.png --output src-tauri/icons
npx tauri build --target "$TARGET" --bundles app,dmg
