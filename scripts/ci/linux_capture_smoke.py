#!/usr/bin/env python3
"""Drives quorum-capture against a live headless PipeWire graph (see linux-capture-smoke.sh).

Subcommands (helper = path to quorum-capture):
  streams                       wait until the zoom and slack tone players are running streams
  list HELPER                   `list` shows both players, neither in a call
  capture HELPER APP PRESENT ABSENT [--no-pw-record]
                                capture APP; PRESENT Hz must be in the call channel, ABSENT Hz not
  detect HELPER                 `detect` reports call_started then call_ended for a recorder
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time

from capture_frames import (
    CHANNEL_CALL,
    channel_samples,
    parse_frames,
    rms,
    tone_amplitude,
    whole_seconds_tail,
)

PRESENT_MIN = 4000.0  # the players use amplitude 12000; a captured tone is far above this
ABSENT_MAX = 400.0  # a tone that is not captured is ~0; leaking another app would be ~12000
NEED_SECONDS = 5  # of call audio to collect
CAPTURE_TIMEOUT = 60  # includes the helper's 6 s pw-record grace and 2 s poll
EXIT_TIMEOUT = 10


def fail(message):
    print(f"FAIL: {message}", flush=True)
    sys.exit(1)


def note(message):
    print(message, flush=True)


def pw_dump_nodes():
    out = subprocess.run(["pw-dump"], capture_output=True, text=True, timeout=20, check=True)
    return [o for o in json.loads(out.stdout) if o.get("type") == "PipeWire:Interface:Node"]


def stream_state():
    found = {}
    for node in pw_dump_nodes():
        info = node.get("info", {})
        props = info.get("props", {})
        if props.get("media.class") == "Stream/Output/Audio":
            found[props.get("application.process.binary", "?")] = info.get("state")
    return found


def cmd_streams():
    deadline = time.time() + 40
    state = {}
    while time.time() < deadline:
        state = stream_state()
        if state.get("zoom") == "running" and state.get("slack") == "running":
            note(f"output streams by application.process.binary: {state}")
            return
        time.sleep(1)
    fail(f"tone players never became running output streams: {state}")


def run_json(helper, *args, env=None):
    out = subprocess.run([helper, *args], capture_output=True, text=True, timeout=30, env=env)
    if out.returncode != 0:
        fail(f"{args} exited {out.returncode}: {out.stderr}")
    try:
        return json.loads(out.stdout)
    except ValueError as e:
        fail(f"{args} did not print one JSON document ({e}): {out.stdout!r}")


def cmd_list(helper):
    entries = run_json(helper, "list")
    if not isinstance(entries, list):
        fail(f"list is not a JSON array: {entries!r}")
    by_id = {e.get("bundle_id"): e for e in entries}
    for app in ("zoom", "slack"):
        if app not in by_id:
            fail(f"list does not show {app}: {entries}")
        if set(by_id[app]) != {"bundle_id", "app", "name", "in_call"}:
            fail(f"unexpected keys in {by_id[app]}")
        if by_id[app]["in_call"] is not False:
            fail(f"{app} is only playing, not recording, so in_call must be false: {entries}")
    note(f"list OK: {entries}")


def read_stderr(proc, lines):
    for raw in proc.stderr:
        lines.append(raw.decode(errors="replace").rstrip())


def cmd_capture(helper, app, present, absent, no_pw_record):
    present, absent = float(present), float(absent)
    env = dict(os.environ)
    shim = None
    if no_pw_record:
        # A PATH that has every tool the helper uses except pw-record, so spawning it fails
        # the way it does on a machine without it. `which` proves the tool is really gone.
        shim = tempfile.mkdtemp()
        for tool in ("pw-dump", "pactl", "parec"):
            path = shutil.which(tool)
            if not path:
                fail(f"{tool} is not installed")
            os.symlink(path, os.path.join(shim, tool))
        env["PATH"] = shim
        if shutil.which("pw-record", path=shim):
            fail("pw-record is still reachable")
    proc = subprocess.Popen(
        [helper, "capture", app],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )
    log = []
    threading.Thread(target=read_stderr, args=(proc, log), daemon=True).start()
    buf = bytearray()

    def pump():
        while True:
            chunk = proc.stdout.read1(65536)
            if not chunk:
                return
            buf.extend(chunk)

    threading.Thread(target=pump, daemon=True).start()
    deadline = time.time() + CAPTURE_TIMEOUT
    call = []
    while time.time() < deadline and proc.poll() is None:
        try:
            frames, _ = parse_frames(bytes(buf))
        except ValueError as e:
            proc.kill()
            fail(f"stdout is not a frame stream: {e}")
        call = channel_samples(frames, CHANNEL_CALL)
        if len(call) >= NEED_SECONDS * 16_000:
            break
        time.sleep(0.5)
    died = proc.poll()
    proc.stdin.close()  # the app going away: the helper must exit by itself
    try:
        code = proc.wait(timeout=EXIT_TIMEOUT)
    except subprocess.TimeoutExpired:
        proc.kill()
        fail("helper did not exit within 10 s of stdin closing")
    stderr = "\n".join(log)
    print(f"--- helper stderr ({app}, no_pw_record={no_pw_record}) ---\n{stderr}\n---", flush=True)
    if died is not None:
        fail(f"helper exited early with {died}")
    if code != 0:
        fail(f"helper exit code {code} after stdin closed, expected 0")
    if len(call) < NEED_SECONDS * 16_000:
        fail(f"only {len(call)} call samples arrived, expected {NEED_SECONDS * 16_000}")
    tail = whole_seconds_tail(call)
    got_present = tone_amplitude(tail, present)
    got_absent = tone_amplitude(tail, absent)
    note(
        f"call channel: {len(call)} samples, rms {rms(tail):.0f}, "
        f"{present:.0f} Hz amplitude {got_present:.0f}, {absent:.0f} Hz amplitude {got_absent:.0f}"
    )
    if got_present < PRESENT_MIN:
        fail(f"the {present:.0f} Hz tone of {app} is not in the call channel")
    if got_absent > ABSENT_MAX:
        fail(f"the {absent:.0f} Hz tone of another app leaked into the {app} capture")
    backends = [ln for ln in log if "recording" in ln and "stream" in ln]
    if not backends:
        fail("helper never logged which recorder it used")
    last = backends[-1]
    note(f"recorder that delivered the audio: {last}")
    if no_pw_record and "(Pulse)" not in last:
        fail(f"pw-record was removed, so the parec fallback must record: {last}")
    if no_pw_record and any("(PipeWire)" in ln and "recording" in ln for ln in log):
        fail("PipeWire recorder was used although pw-record was removed")
    print("::notice::capture OK: backend=" + last.rsplit("(", 1)[-1].rstrip(")"), flush=True)


def read_json_line(proc, deadline):
    while time.time() < deadline:
        line = proc.stdout.readline()
        if line:
            return json.loads(line)
    fail("no detect event before the deadline")


def cmd_detect(helper):
    rec = os.environ["QC_RECORDER"]  # a copy of parec named zoom, started here
    proc = subprocess.Popen(
        [helper, "detect"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE
    )
    recorder = subprocess.Popen(
        [rec, "--record", "--raw", "--format=s16le", "--rate=16000", "--channels=1",
         "--device=ci_mic.monitor"],
        stdout=subprocess.DEVNULL,
    )
    try:
        started = read_json_line(proc, time.time() + 30)
        if started != {"event": "call_started", "bundle_id": "zoom", "app": "zoom"}:
            fail(f"unexpected first event {started}")
        note(f"detect: {started}")
        entries = run_json(helper, "list")
        zoom = [e for e in entries if e.get("bundle_id") == "zoom"]
        if not zoom or zoom[0]["in_call"] is not True:
            fail(f"list does not show zoom in a call while it records: {entries}")
        recorder.terminate()
        recorder.wait(timeout=10)
        ended = read_json_line(proc, time.time() + 30)
        if ended != {"event": "call_ended", "bundle_id": "zoom", "app": "zoom"}:
            fail(f"unexpected second event {ended}")
        note(f"detect: {ended}")
    finally:
        recorder.kill()
        proc.stdin.close()
        try:
            proc.wait(timeout=EXIT_TIMEOUT)
        except subprocess.TimeoutExpired:
            proc.kill()
            fail("detect did not exit when stdin closed")


def main(argv):
    if argv[:1] == ["streams"]:
        cmd_streams()
    elif argv[:1] == ["list"] and len(argv) == 2:
        cmd_list(argv[1])
    elif argv[:1] == ["capture"] and len(argv) in (5, 6):
        cmd_capture(*argv[1:5], no_pw_record=argv[5:] == ["--no-pw-record"])
    elif argv[:1] == ["detect"] and len(argv) == 2:
        cmd_detect(argv[1])
    else:
        fail(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
