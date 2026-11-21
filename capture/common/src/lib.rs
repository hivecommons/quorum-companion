//! The wire contract between the capture helpers and the Tauri host (`src-tauri/src/audio.rs`):
//! the 16 kHz target rate, the channel bytes, s16le encoding, the resampler and the stdout
//! frame layout. Platform-neutral, shared by the Linux and Windows helpers so they cannot drift.

pub mod stdio;

/// The sample rate the companion streams.
pub const TARGET_RATE: u32 = 16_000;
/// Frame channel byte for the meeting app's audio.
pub const CHANNEL_CALL: u8 = 0;
/// Frame channel byte for the microphone.
pub const CHANNEL_MIC: u8 = 1;

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

    fn samples(bytes: &[u8]) -> Vec<i16> {
        bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect()
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
    fn frames_match_the_app_reader() {
        assert_eq!(frame(CHANNEL_MIC, &[9, 8]), [1, 2, 0, 0, 0, 9, 8]);
        assert_eq!(frame(CHANNEL_CALL, &[]), [0, 0, 0, 0, 0]);
    }

    #[test]
    fn frame_length_is_a_little_endian_u32_and_the_payload_is_verbatim() {
        // 70_000 = 0x0001_1170 needs three length bytes, so byte order and width both matter.
        let payload: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let framed = frame(CHANNEL_CALL, &payload);
        assert_eq!(framed.len(), 5 + payload.len());
        assert_eq!(framed[0], CHANNEL_CALL);
        assert_eq!(framed[1..5], [0x70, 0x11, 0x01, 0x00]);
        assert_eq!(framed[5..], payload[..]);
        assert_eq!(frame(CHANNEL_MIC, &[1])[0], 1);
        assert_ne!(CHANNEL_CALL, CHANNEL_MIC);
    }
}
