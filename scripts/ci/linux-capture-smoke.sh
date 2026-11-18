#!/usr/bin/env bash
# Real-audio smoke test of the Linux capture helper against a headless PipeWire + WirePlumber +
# pipewire-pulse started here, with a null sink standing in for the speakers. Run it inside
# `dbus-run-session --`. Usage: linux-capture-smoke.sh <path to quorum-capture>
#
# What is real: PipeWire, WirePlumber, pipewire-pulse, the pw-dump / pw-record / pactl / parec
# tools, the helper binary, and the audio data (two sine tones played by two processes).
# What is not: there is no sound card and no actual meeting app. The tone players are copies of
# `pacat` named zoom and slack, so PipeWire tags their streams application.process.binary=zoom
# and slack, which is how the helper recognises a meeting app.
set -euo pipefail

helper="$(realpath "$1")"
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
export XDG_RUNTIME_DIR="$work/run"
mkdir -m 700 "$XDG_RUNTIME_DIR"
export PYTHONDONTWRITEBYTECODE=1

dump_logs() {
  for f in "$work"/*.log; do
    echo "::group::$(basename "$f")"; cat "$f" || true; echo "::endgroup::"
  done
}
cleanup() {
  status=$?
  [ "$status" -eq 0 ] || dump_logs
  pkill -f "$work/bin/" 2>/dev/null || true
  kill $(jobs -p) 2>/dev/null || true
  exit "$status"
}
trap cleanup EXIT

echo "PipeWire: $(pipewire --version | tr '\n' ' ')"
echo "WirePlumber: $(wireplumber --version 2>&1 | head -n1)"
echo "pw-record: $(pw-record --version | tr '\n' ' ')"

start_audio() {
  pipewire > "$work/pipewire.log" 2>&1 &
  echo $! > "$work/pids"
  sleep 1
  wireplumber > "$work/wireplumber.log" 2>&1 &
  echo $! >> "$work/pids"
  pipewire-pulse > "$work/pipewire-pulse.log" 2>&1 &
  echo $! >> "$work/pids"
}
stop_audio() {
  xargs -r kill < "$work/pids" 2>/dev/null || true
  sleep 1
}

# The only retry in this script: runner audio startup (the daemons race each other).
ready=0
for attempt in 1 2 3; do
  start_audio
  for _ in $(seq 1 20); do
    if pactl info > /dev/null 2>&1; then ready=1; break; fi
    sleep 1
  done
  [ "$ready" -eq 1 ] && break
  echo "audio startup attempt $attempt failed, retrying"
  stop_audio
done
[ "$ready" -eq 1 ] || { echo "FAIL: PipeWire never became ready"; exit 1; }
pactl info | grep -E 'Server Name|Server Version'

pactl load-module module-null-sink sink_name=ci_sink sink_properties=device.description=ci_sink > /dev/null
pactl load-module module-null-sink sink_name=ci_mic sink_properties=device.description=ci_mic > /dev/null
pactl set-default-sink ci_sink
pactl set-default-source ci_mic.monitor # silent "microphone": nothing plays into ci_mic

# Tone players: copies of pacat under meeting app names (pacat's mode is set with flags).
mkdir "$work/bin"
pacat_bin="$(readlink -f "$(command -v pacat)")"
cp "$pacat_bin" "$work/bin/zoom"
cp "$pacat_bin" "$work/bin/slack"
python3 - "$work" <<'PY'
import math, struct, sys
work = sys.argv[1]
for name, freq in (("zoom", 440), ("slack", 1000)):
    # 10 s of stereo 48 kHz s16le: whole cycles, so looping it is seamless.
    frames = bytearray()
    for n in range(48_000 * 10):
        v = int(12_000 * math.sin(2 * math.pi * freq * n / 48_000))
        frames += struct.pack("<hh", v, v)
    open(f"{work}/{name}.raw", "wb").write(frames)
PY
play() { # <binary> <raw file>
  ( while :; do cat "$2"; done | "$work/bin/$1" --playback --raw --format=s16le \
      --rate=48000 --channels=2 --device=ci_sink ) > "$work/play-$1.log" 2>&1 &
}
play zoom "$work/zoom.raw"
play slack "$work/slack.raw"

py() { python3 "$here/linux_capture_smoke.py" "$@"; }

echo "::group::streams"; py streams; echo "::endgroup::"
echo "::group::list"; py list "$helper"; echo "::endgroup::"
echo "::group::capture zoom (default recorder: pw-record, with the helper's own fallback)"
py capture "$helper" zoom 440 1000
echo "::endgroup::"
echo "::group::capture slack (positive control: the leak check can see the other tone)"
py capture "$helper" slack 1000 440
echo "::endgroup::"
echo "::group::capture zoom with pw-record removed (parec fallback)"
py capture "$helper" zoom 440 1000 --no-pw-record
echo "::endgroup::"
echo "::group::detect"
# The recorder is the zoom copy of pacat again, in record mode.
QC_RECORDER="$work/bin/zoom" py detect "$helper"
echo "::endgroup::"
echo "linux capture smoke test passed"
