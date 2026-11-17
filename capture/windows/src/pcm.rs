//! PCM conversion to what hive and Quorum take (16 kHz mono s16le) and the helper's stdout
//! frames. Plain logic, tested on every platform.
//!
//! WASAPI hands the helper interleaved buffers in the device's mix format (usually 32-bit float,
//! two or more channels, 44.1 or 48 kHz) or in the format the helper asked for. Each buffer is
//! downmixed to mono, resampled to 16 kHz and written as one frame.

/// The sample rate the companion streams.
pub const TARGET_RATE: u32 = 16_000;
/// Frame channel byte for the meeting app's audio.
pub const CHANNEL_CALL: u8 = 0;
/// Frame channel byte for the microphone.
pub const CHANNEL_MIC: u8 = 1;

const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// How one sample is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleKind {
    I16,
    /// 32-bit integer container (also 24 valid bits, which are left-aligned).
    I32,
    F32,
}

impl SampleKind {
    pub fn bytes(self) -> usize {
        match self {
            SampleKind::I16 => 2,
            SampleKind::I32 | SampleKind::F32 => 4,
        }
    }

    /// The kind for a `WAVEFORMATEX` tag and bit depth; for `WAVE_FORMAT_EXTENSIBLE`,
    /// `sub_format` is the first field (`Data1`) of the `SubFormat` GUID (1 = PCM, 3 = float).
    pub fn from_wave(tag: u16, bits: u16, sub_format: Option<u32>) -> Option<SampleKind> {
        let tag = match (tag, sub_format) {
            (WAVE_FORMAT_EXTENSIBLE, Some(sub)) => u16::try_from(sub).ok()?,
            (WAVE_FORMAT_EXTENSIBLE, None) => return None,
            (tag, _) => tag,
        };
        match (tag, bits) {
            (WAVE_FORMAT_PCM, 16) => Some(SampleKind::I16),
            (WAVE_FORMAT_PCM, 32) => Some(SampleKind::I32),
            (WAVE_FORMAT_IEEE_FLOAT, 32) => Some(SampleKind::F32),
            _ => None,
        }
    }
}

/// An interleaved PCM stream's layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub kind: SampleKind,
    pub channels: u16,
    pub rate: u32,
}

impl Format {
    /// Bytes per frame (one sample of every channel).
    pub fn frame_bytes(self) -> usize {
        self.kind.bytes() * usize::from(self.channels.max(1))
    }
}

fn sample(bytes: &[u8], kind: SampleKind) -> f32 {
    match kind {
        SampleKind::I16 => f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32_768.0,
        SampleKind::I32 => {
            let v = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            (f64::from(v) / 2_147_483_648.0) as f32
        }
        SampleKind::F32 => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
    }
}

/// Downmixes interleaved PCM to mono samples in [-1, 1] by averaging the channels. A trailing
/// partial frame is ignored.
pub fn to_mono(bytes: &[u8], format: Format) -> Vec<f32> {
    let width = format.kind.bytes();
    let channels = usize::from(format.channels.max(1));
    bytes
        .chunks_exact(format.frame_bytes())
        .map(|frame| {
            let sum: f32 = frame
                .chunks_exact(width)
                .map(|s| sample(s, format.kind))
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

    #[test]
    fn wave_formats_map_to_sample_kinds() {
        assert_eq!(SampleKind::from_wave(1, 16, None), Some(SampleKind::I16));
        assert_eq!(SampleKind::from_wave(3, 32, None), Some(SampleKind::F32));
        assert_eq!(
            SampleKind::from_wave(0xFFFE, 32, Some(3)),
            Some(SampleKind::F32)
        );
        assert_eq!(
            SampleKind::from_wave(0xFFFE, 32, Some(1)),
            Some(SampleKind::I32)
        );
        assert_eq!(SampleKind::from_wave(0xFFFE, 32, None), None);
        assert_eq!(SampleKind::from_wave(1, 8, None), None);
        assert_eq!(SampleKind::from_wave(3, 64, None), None);
    }

    #[test]
    fn stereo_s16_downmixes_to_the_mean() {
        let format = Format {
            kind: SampleKind::I16,
            channels: 2,
            rate: 48_000,
        };
        assert_eq!(format.frame_bytes(), 4);
        let mut bytes = Vec::new();
        for (l, r) in [(16_384i16, 0i16), (-32_768, -32_768), (100, -100)] {
            bytes.extend_from_slice(&l.to_le_bytes());
            bytes.extend_from_slice(&r.to_le_bytes());
        }
        bytes.push(7); // a partial frame is ignored
        assert!(close(&to_mono(&bytes, format), &[0.25, -1.0, 0.0]));
    }

    #[test]
    fn float_and_i32_samples_read_in_range() {
        let f32_format = Format {
            kind: SampleKind::F32,
            channels: 1,
            rate: 48_000,
        };
        let mut bytes = Vec::new();
        for v in [0.5f32, -0.25] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        assert!(close(&to_mono(&bytes, f32_format), &[0.5, -0.25]));
        let i32_format = Format {
            kind: SampleKind::I32,
            ..f32_format
        };
        let bytes = (i32::MIN / 2).to_le_bytes();
        assert!(close(&to_mono(&bytes, i32_format), &[-0.5]));
    }

    #[test]
    fn s16le_clamps_and_rounds() {
        let bytes = to_s16le(&[0.0, 1.0, -1.0, 2.0, -2.0, f32::NAN, 0.5]);
        let values: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(values, vec![0, 32_767, -32_767, 32_767, -32_767, 0, 16_384]);
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
        assert_eq!(frame(CHANNEL_MIC, &[9, 8]), vec![1, 2, 0, 0, 0, 9, 8]);
        assert_eq!(frame(CHANNEL_CALL, &[]), vec![0, 0, 0, 0, 0]);
    }
}
