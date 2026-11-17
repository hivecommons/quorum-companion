//! PCM conversion to what hive and Quorum take (16 kHz mono s16le) and the helper's stdout
//! frames. Plain logic, tested on every platform.
//!
//! The recorders (`pw-record`, or `parec` on pipewire-pulse) hand the helper interleaved s16le
//! at 48 kHz, the usual PipeWire graph rate, so PipeWire does no extra resampling of its own.
//! The helper downmixes to mono, resamples to 16 kHz and writes each read as one frame.

/// The sample rate the companion streams.
pub const TARGET_RATE: u32 = 16_000;
/// Frame channel byte for the meeting app's audio.
pub const CHANNEL_CALL: u8 = 0;
/// Frame channel byte for the microphone.
pub const CHANNEL_MIC: u8 = 1;

const SAMPLE_BYTES: usize = 2;

/// Downmixes interleaved s16le to mono samples in [-1, 1] by averaging the channels. A trailing
/// partial frame is ignored.
pub fn to_mono(bytes: &[u8], channels: usize) -> Vec<f32> {
    let channels = channels.max(1);
    bytes
        .chunks_exact(SAMPLE_BYTES * channels)
        .map(|frame| {
            let sum: f32 = frame
                .chunks_exact(SAMPLE_BYTES)
                .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32_768.0)
                .sum();
            sum / channels as f32
        })
        .collect()
}

/// Mono samples to s16le bytes, clamping to the 16-bit range.
pub fn to_s16le(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let s = if s.is_nan() { 0.0 } else { s.clamp(-1.0, 1.0) };
        let v = (s * 32_767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// A streaming resampler that averages the input over each output sample's span (an area or
/// box filter). Going down from 48 or 44.1 kHz to 16 kHz this is a simple anti-alias filter,
/// enough for speech recognition; 48 to 16 kHz is the plain mean of every three samples.
/// State carries over between buffers, so splitting the input anywhere gives the same output.
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Where the next output sample starts, in `pending` sample units.
    pos: f64,
    pending: Vec<f32>,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Resampler {
        Resampler {
            step: f64::from(from_rate.max(1)) / f64::from(to_rate.max(1)),
            pos: 0.0,
            pending: Vec::new(),
        }
    }

    pub fn push(&mut self, input: &[f32]) -> Vec<f32> {
        self.pending.extend_from_slice(input);
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 1);
        loop {
            let start = self.pos;
            let end = start + self.step;
            if end.ceil() as usize > self.pending.len() {
                break;
            }
            out.push((self.area(start, end) / self.step) as f32);
            self.pos = end;
        }
        let consumed = (self.pos.floor() as usize).min(self.pending.len());
        self.pending.drain(..consumed);
        self.pos -= consumed as f64;
        out
    }

    /// The integral of the step function `pending[i]` on `[i, i + 1)` over `[start, end)`.
    fn area(&self, start: f64, end: f64) -> f64 {
        let first = start.floor() as usize;
        let last = (end.ceil() as usize).min(self.pending.len());
        let mut total = 0.0;
        for (i, value) in self.pending[first..last].iter().enumerate() {
            let lo = (first + i) as f64;
            let overlap = end.min(lo + 1.0) - start.max(lo);
            if overlap > 0.0 {
                total += f64::from(*value) * overlap;
            }
        }
        total
    }
}

/// Turns a recorder's raw s16le stream (read in arbitrary pieces) into 16 kHz mono s16le. A
/// piece that ends inside a frame keeps the rest for the next one.
pub struct Converter {
    channels: usize,
    leftover: Vec<u8>,
    resampler: Resampler,
}

impl Converter {
    pub fn new(rate: u32, channels: u16) -> Converter {
        Converter {
            channels: usize::from(channels.max(1)),
            leftover: Vec::new(),
            resampler: Resampler::new(rate, TARGET_RATE),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.leftover.extend_from_slice(bytes);
        let frame_bytes = SAMPLE_BYTES * self.channels;
        let whole = self.leftover.len() - self.leftover.len() % frame_bytes;
        let mono = to_mono(&self.leftover[..whole], self.channels);
        self.leftover.drain(..whole);
        to_s16le(&self.resampler.push(&mono))
    }
}

/// One helper frame: channel byte, little-endian u32 length, PCM (src-tauri/src/audio.rs).
pub fn frame(channel: u8, pcm: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + pcm.len());
    out.push(channel);
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &[f32], b: &[f32]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    fn s16(values: &[i16]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn samples(bytes: &[u8]) -> Vec<i16> {
        bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect()
    }

    #[test]
    fn stereo_downmixes_to_the_mean() {
        let mut bytes = s16(&[16_384, 0, -32_768, -32_768, 100, -100]);
        bytes.push(7); // a partial frame is ignored
        assert!(close(&to_mono(&bytes, 2), &[0.25, -1.0, 0.0]));
        assert!(close(&to_mono(&s16(&[16_384]), 1), &[0.5]));
        assert!(close(&to_mono(&s16(&[16_384]), 0), &[0.5]));
    }

    #[test]
    fn s16le_clamps_and_rounds() {
        let bytes = to_s16le(&[0.0, 1.0, -1.0, 2.0, -2.0, f32::NAN, 0.5]);
        let expected = [0, 32_767, -32_767, 32_767, -32_767, 0, 16_384];
        assert_eq!(samples(&bytes), expected);
    }

    #[test]
    fn resampling_48k_to_16k_averages_threes() {
        let mut r = Resampler::new(48_000, TARGET_RATE);
        let out = r.push(&[3.0, 0.0, 0.0, 1.0, 1.0, 1.0, 9.0]);
        assert!(close(&out, &[1.0, 1.0]));
        // The leftover sample joins the next buffer.
        assert!(close(&r.push(&[0.0, 0.0]), &[3.0]));
    }

    #[test]
    fn resampling_keeps_a_constant_and_the_duration() {
        let mut r = Resampler::new(44_100, TARGET_RATE);
        let mut out = Vec::new();
        // One second in uneven buffers.
        for len in [441usize, 1_000, 7, 42_652] {
            out.extend(r.push(&vec![0.5; len]));
        }
        assert!((15_998..=16_000).contains(&out.len()), "{}", out.len());
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-5));
    }

    #[test]
    fn resampling_does_not_depend_on_buffer_boundaries() {
        let input: Vec<f32> = (0..4_800).map(|i| ((i as f32) * 0.01).sin()).collect();
        let mut whole = Resampler::new(48_000, TARGET_RATE);
        let a = whole.push(&input);
        let mut split = Resampler::new(48_000, TARGET_RATE);
        let mut b = Vec::new();
        for part in input.chunks(97) {
            b.extend(split.push(part));
        }
        assert_eq!(a.len(), 1_600);
        assert!(close(&a, &b));
    }

    #[test]
    fn same_rate_passes_through() {
        let mut r = Resampler::new(TARGET_RATE, TARGET_RATE);
        assert!(close(&r.push(&[0.1, 0.2, 0.3]), &[0.1, 0.2, 0.3]));
    }

    #[test]
    fn converter_gives_16k_mono_whatever_the_read_sizes() {
        // 30 ms of 48 kHz stereo: 1440 frames of 4 bytes.
        let values: Vec<i16> = (0..2_880)
            .map(|i| ((i * 37) % 2_000) as i16 - 1_000)
            .collect();
        let raw = s16(&values);
        let mut whole = Converter::new(48_000, 2);
        let a = whole.push(&raw);
        let mut pieces = Converter::new(48_000, 2);
        let mut b = Vec::new();
        // Odd piece sizes split samples and frames.
        for piece in raw.chunks(13) {
            b.extend(pieces.push(piece));
        }
        assert_eq!(a.len(), 480 * 2);
        assert_eq!(a, b);
    }

    #[test]
    fn converter_keeps_a_partial_frame_for_the_next_read() {
        let mut c = Converter::new(TARGET_RATE, 2);
        let raw = s16(&[1_000, 3_000, -2_000, -2_000]);
        assert!(c.push(&raw[..3]).is_empty());
        let out = c.push(&raw[3..]);
        assert_eq!(samples(&out).len(), 2);
        assert!((samples(&out)[0] - 2_000).abs() <= 1);
        assert!((samples(&out)[1] + 2_000).abs() <= 1);
    }

    #[test]
    fn frames_match_the_app_reader() {
        assert_eq!(frame(CHANNEL_MIC, &[9, 8]), [1, 2, 0, 0, 0, 9, 8]);
        assert_eq!(frame(CHANNEL_CALL, &[]), [0, 0, 0, 0, 0]);
    }
}
