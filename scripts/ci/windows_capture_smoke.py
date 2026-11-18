#!/usr/bin/env python3
"""Smoke test of quorum-capture.exe on a hosted Windows runner.

Usage: windows_capture_smoke.py HELPER MODE        MODE is `audio` or `degraded`.

MODE comes from the probe step in ci.yml (does the runner have a render endpoint?):

  audio     A render endpoint exists. Two child processes named zoom.exe and slack.exe (copies of
            powershell.exe) play 440 Hz and 1000 Hz tones; `capture zoom.exe` must deliver call
            frames that contain 440 Hz and not 1000 Hz. This is a real process loopback capture.
  degraded  No render endpoint. Nothing here captures audio, and the log says so. It checks what
            can be checked: `list` against a real process snapshot (a copy of ping.exe named
            zoom.exe), the argument and not-running error paths, and that `capture zoom.exe`
            either reports a specific activation error (exit 2) or, if Windows still hands out a
            loopback client, starts and stops cleanly, and in neither case crashes.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import wave
import math
import struct

from capture_frames import (
    CHANNEL_CALL,
    channel_samples,
    parse_frames,
    rms,
    tone_amplitude,
    whole_seconds_tail,
)

PRESENT_MIN = 4000.0
ABSENT_MAX = 400.0
NEED_SECONDS = 5
CAPTURE_TIMEOUT = 40
EXIT_TIMEOUT = 15
DEGRADED_WAIT = 12
SYSTEM32 = os.path.join(os.environ.get("SystemRoot", r"C:\Windows"), "System32")
POWERSHELL = os.path.join(SYSTEM32, "WindowsPowerShell", "v1.0", "powershell.exe")
PING = os.path.join(SYSTEM32, "PING.EXE")
# Windows reports a crash as an NTSTATUS exit code (0xC0000005 and the like); Rust panics exit 101.
CRASH_CODES = {101}


def fail(message):
    print(f"FAIL: {message}", flush=True)
    sys.exit(1)


def note(message):
    print(message, flush=True)


def is_crash(code):
    return code in CRASH_CODES or code < 0 or code >= 0xC0000000


class Helper:
    def __init__(self, helper, *args):
        self.proc = subprocess.Popen(
            [helper, *args],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.buf = bytearray()
        self.log = []
        threading.Thread(target=self._pump_out, daemon=True).start()
        threading.Thread(target=self._pump_err, daemon=True).start()

    def _pump_out(self):
        while True:
            chunk = self.proc.stdout.read1(65536)
            if not chunk:
                return
            self.buf.extend(chunk)

    def _pump_err(self):
        for raw in self.proc.stderr:
            self.log.append(raw.decode(errors="replace").rstrip())

    def stderr(self):
        return "\n".join(self.log)

    def finish(self, timeout=EXIT_TIMEOUT):
        """Closes stdin (the app going away) and returns the exit code."""
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            code = self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            fail("helper did not exit after stdin closed")
        time.sleep(0.5)  # let the reader threads drain
        return code


def run(helper, *args):
    h = Helper(helper, *args)
    code = h.finish()
    return code, bytes(h.buf), h.stderr()


def copy_exe(src, dst_dir, name):
    dst = os.path.join(dst_dir, name)
    shutil.copy(src, dst)
    return dst


def check_list_shape(entries):
    if not isinstance(entries, list):
        fail(f"list is not a JSON array: {entries!r}")
    for e in entries:
        if set(e) != {"bundle_id", "app", "name", "in_call"}:
            fail(f"unexpected keys in {e}")
        if not isinstance(e["in_call"], bool):
            fail(f"in_call is not a bool in {e}")


def test_usage_errors(helper):
    code, out, err = run(helper)
    if code != 64 or "usage:" not in err or out:
        fail(f"no arguments: exit {code}, stderr {err!r}, stdout {out!r}")
    code, out, err = run(helper, "capture")
    if code != 64 or "usage:" not in err:
        fail(f"capture without an app: exit {code}, stderr {err!r}")
    # stdin stays open: the helper exits 0 as soon as stdin closes, which would race the check.
    h = Helper(helper, "capture", "definitely-not-running.exe")
    try:
        code = h.proc.wait(timeout=EXIT_TIMEOUT)
    except subprocess.TimeoutExpired:
        h.proc.kill()
        fail("capture of a missing app did not exit by itself")
    time.sleep(0.5)
    err = h.stderr()
    if code != 2 or "not running" not in err:
        fail(f"capture of a process that is not running: exit {code}, stderr {err!r}")
    note("usage error paths OK: exit 64 for bad arguments, exit 2 'not running' for a missing app")


def write_tone(path, freq):
    with wave.open(path, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(48_000)
        frames = bytearray()
        for n in range(48_000 * 10):
            v = int(12_000 * math.sin(2 * math.pi * freq * n / 48_000))
            frames += struct.pack("<hh", v, v)
        w.writeframes(bytes(frames))


def start_player(tmp, name, freq):
    exe = copy_exe(POWERSHELL, tmp, name)
    wav = os.path.join(tmp, name.replace(".exe", ".wav"))
    write_tone(wav, freq)
    script = (
        f"$p = New-Object System.Media.SoundPlayer '{wav}'; while ($true) {{ $p.PlaySync() }}"
    )
    return subprocess.Popen([exe, "-NoProfile", "-NonInteractive", "-Command", script])


def test_audio(helper, tmp):
    zoom = start_player(tmp, "zoom.exe", 440)
    slack = start_player(tmp, "slack.exe", 1000)
    players = [zoom, slack]
    try:
        time.sleep(5)  # let both players open their render streams
        for p in players:
            if p.poll() is not None:
                fail(f"a tone player exited early with {p.returncode}")
        code, out, err = run(helper, "list")
        entries = json.loads(out)
        check_list_shape(entries)
        ids = {e["bundle_id"] for e in entries}
        if not {"zoom.exe", "slack.exe"} <= ids:
            fail(f"list does not show both players: {entries}")
        note(f"list OK: {entries}")

        h = Helper(helper, "capture", "zoom.exe")
        deadline = time.time() + CAPTURE_TIMEOUT
        call = []
        while time.time() < deadline and h.proc.poll() is None:
            try:
                frames, _ = parse_frames(bytes(h.buf))
            except ValueError as e:
                h.proc.kill()
                fail(f"stdout is not a frame stream: {e}")
            call = channel_samples(frames, CHANNEL_CALL)
            if len(call) >= NEED_SECONDS * 16_000:
                break
            time.sleep(0.5)
        early = h.proc.poll()
        code = h.finish()
        print(f"--- helper stderr ---\n{h.stderr()}\n---", flush=True)
        if early is not None:
            fail(f"helper exited early with {early}")
        if code != 0:
            fail(f"helper exit code {code} after stdin closed, expected 0")
        if len(call) < NEED_SECONDS * 16_000:
            fail(f"only {len(call)} call samples arrived, expected {NEED_SECONDS * 16_000}")
        tail = whole_seconds_tail(call)
        got_440 = tone_amplitude(tail, 440)
        got_1000 = tone_amplitude(tail, 1000)
        note(
            f"call channel: {len(call)} samples, rms {rms(tail):.0f}, "
            f"440 Hz {got_440:.0f}, 1000 Hz {got_1000:.0f}"
        )
        if got_440 < PRESENT_MIN:
            fail("the 440 Hz tone of zoom.exe is not in the call channel")
        if got_1000 > ABSENT_MAX:
            fail("the 1000 Hz tone of slack.exe leaked into the zoom.exe capture")
        print("::notice::windows capture smoke test: REAL process loopback capture passed", flush=True)
    finally:
        for p in players:
            p.kill()


def test_degraded(helper, tmp):
    print(
        "::warning::windows capture smoke test: DEGRADED mode, no audio endpoint on this "
        "runner; no audio was captured, only list shape and error paths were checked",
        flush=True,
    )
    test_usage_errors(helper)
    code, out, err = run(helper, "list")
    if code != 0:
        fail(f"list exited {code}: {err}")
    check_list_shape(json.loads(out))
    note(f"list with no meeting app running OK: {out.decode().strip()}")

    ping = copy_exe(PING, tmp, "zoom.exe")
    sleeper = subprocess.Popen([ping, "-n", "300", "127.0.0.1"], stdout=subprocess.DEVNULL)
    try:
        time.sleep(1)
        code, out, err = run(helper, "list")
        entries = json.loads(out)
        check_list_shape(entries)
        zoom = [e for e in entries if e["bundle_id"] == "zoom.exe"]
        if code != 0 or len(zoom) != 1 or zoom[0]["app"] != "zoom" or zoom[0]["in_call"]:
            fail(f"list does not show a running zoom.exe that is not in a call: {entries}")
        note(f"list OK against a real process snapshot: {zoom[0]}")

        h = Helper(helper, "capture", "zoom.exe")
        deadline = time.time() + DEGRADED_WAIT
        while time.time() < deadline and h.proc.poll() is None:
            time.sleep(0.5)
        exited = h.proc.poll()
        err = h.stderr()
        code = exited if exited is not None else h.finish()
        err = h.stderr()
        print(f"--- helper stderr ---\n{err}\n---", flush=True)
        if "panicked" in err or is_crash(code):
            fail(f"helper crashed (exit {code})")
        try:
            parse_frames(bytes(h.buf))
        except ValueError as e:
            fail(f"stdout is not a frame stream: {e}")
        if exited is None:
            if code != 0:
                fail(f"helper was running, then exited {code} after stdin closed, expected 0")
            outcome = "capture started without a render endpoint and stopped cleanly"
        elif code == 2 and "cannot capture zoom.exe (pid" in err:
            outcome = "process loopback activation refused with a specific error (exit 2)"
        elif code == 3 and "stopped:" in err:
            outcome = "capture started and then reported why it stopped (exit 3)"
        else:
            fail(f"unexpected exit {code} without a specific error message")
        note(f"capture activation result: {outcome}")
        print(f"::notice::degraded mode capture outcome: {outcome}", flush=True)
    finally:
        sleeper.kill()


def main(argv):
    if len(argv) != 2 or argv[1] not in ("audio", "degraded"):
        fail(__doc__)
    helper = os.path.abspath(argv[0])
    tmp = tempfile.mkdtemp()
    if argv[1] == "audio":
        test_usage_errors(helper)
        test_audio(helper, tmp)
    else:
        test_degraded(helper, tmp)


if __name__ == "__main__":
    main(sys.argv[1:])
