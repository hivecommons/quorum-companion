"""Shared helpers for the CI capture smoke tests: parse the quorum-capture frame stream and
measure tones in it. Standard library only.

Frame format (same as src-tauri/src/audio.rs): 1 byte channel (0 = call, 1 = mic), 4 bytes
little-endian length, then that many bytes of 16 kHz mono s16le PCM.
"""
import math
import struct

CHANNEL_CALL = 0
CHANNEL_MIC = 1
RATE = 16_000
HEADER = 5


def parse_frames(buf):
    """Returns (frames, leftover): frames is a list of (channel, payload). A truncated last
    frame is left in `leftover`. Raises ValueError for a channel byte that is not 0 or 1, which
    means the stream is out of sync (something other than frames reached stdout)."""
    frames = []
    pos = 0
    while len(buf) - pos >= HEADER:
        channel = buf[pos]
        if channel not in (CHANNEL_CALL, CHANNEL_MIC):
            raise ValueError(f"bad channel byte {channel} at offset {pos}")
        (length,) = struct.unpack_from("<I", buf, pos + 1)
        if len(buf) - pos - HEADER < length:
            break
        frames.append((channel, bytes(buf[pos + HEADER : pos + HEADER + length])))
        pos += HEADER + length
    return frames, bytes(buf[pos:])


def channel_samples(frames, channel):
    data = b"".join(p for c, p in frames if c == channel)
    if len(data) % 2:
        raise ValueError("odd number of PCM bytes: not s16le")
    return list(struct.unpack(f"<{len(data) // 2}h", data))


def tone_amplitude(samples, freq, rate=RATE):
    """Peak amplitude (in s16 units) of a sine at `freq` Hz in `samples` (Goertzel)."""
    n = len(samples)
    if n == 0:
        return 0.0
    coeff = 2.0 * math.cos(2.0 * math.pi * freq / rate)
    s1 = s2 = 0.0
    for x in samples:
        s0 = x + coeff * s1 - s2
        s2, s1 = s1, s0
    power = s1 * s1 + s2 * s2 - coeff * s1 * s2
    return 2.0 * math.sqrt(max(power, 0.0)) / n


def whole_seconds_tail(samples, max_seconds=3):
    """The last whole seconds (up to max_seconds) so tones of integer Hz fit whole cycles."""
    seconds = min(len(samples) // RATE, max_seconds)
    return samples[len(samples) - seconds * RATE :] if seconds else []


def rms(samples):
    return math.sqrt(sum(x * x for x in samples) / len(samples)) if samples else 0.0
