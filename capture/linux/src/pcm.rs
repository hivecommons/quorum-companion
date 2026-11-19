//! PCM conversion to what hive and Quorum take (16 kHz mono s16le) and the helper's stdout
//! frames. Plain logic, tested on every platform.
//!
//! The recorders (`pw-record`, or `parec` on pipewire-pulse) hand the helper interleaved s16le
//! at 48 kHz, the usual PipeWire graph rate, so PipeWire does no extra resampling of its own.
//! The helper downmixes to mono, resamples to 16 kHz and writes each read as one frame.

pub use quorum_capture_common::{
    frame, to_s16le, Resampler, CHANNEL_CALL, CHANNEL_MIC, TARGET_RATE,
};

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
}
