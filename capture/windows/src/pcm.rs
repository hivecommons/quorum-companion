//! PCM conversion to what hive and Quorum take (16 kHz mono s16le) and the helper's stdout
//! frames. Plain logic, tested on every platform.
//!
//! WASAPI hands the helper interleaved buffers in the device's mix format (usually 32-bit float,
//! two or more channels, 44.1 or 48 kHz) or in the format the helper asked for. Each buffer is
//! downmixed to mono, resampled to 16 kHz and written as one frame.

pub use quorum_capture_common::{
    frame, to_s16le, Resampler, CHANNEL_CALL, CHANNEL_MIC, TARGET_RATE,
};

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
}
