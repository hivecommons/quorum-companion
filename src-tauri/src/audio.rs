//! The capture helper's frame stream and the chunks sent to the hive.
//!
//! The per-OS capture helper (capture/macos for v1) writes frames on stdout: one byte channel
//! (0 = the meeting app's audio, 1 = the microphone), a little-endian u32 length, then 16 kHz
//! mono s16le PCM. The chunker groups each channel's PCM into chunks of `CHUNK_BYTES`, the unit
//! of `POST /api/companion/meetings/{id}/audio`.

use std::io::{self, Read};

/// One second of 16 kHz mono 16-bit audio.
pub const CHUNK_BYTES: usize = 32_000;
/// The largest frame the helper may send; anything bigger means the stream is corrupt.
pub const MAX_FRAME_BYTES: usize = 1 << 20;
const HEADER_BYTES: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    Call,
    Mic,
}

impl Channel {
    pub fn from_byte(b: u8) -> Option<Channel> {
        match b {
            0 => Some(Channel::Call),
            1 => Some(Channel::Mic),
            _ => None,
        }
    }

    /// The `channel` query value hive takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Call => "call",
            Channel::Mic => "mic",
        }
    }
}

/// Reads the next frame; `Ok(None)` at a clean end of stream.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<(Channel, Vec<u8>)>> {
    let mut header = [0u8; HEADER_BYTES];
    match r.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let channel = Channel::from_byte(header[0])
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unknown channel"))?;
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut pcm = vec![0u8; len];
    r.read_exact(&mut pcm)?;
    Ok(Some((channel, pcm)))
}

/// Groups one channel's PCM into fixed-size chunks; `flush` returns the rest (whole samples).
#[derive(Default)]
pub struct Chunker {
    buf: Vec<u8>,
}

impl Chunker {
    pub fn push(&mut self, pcm: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(pcm);
        let mut out = Vec::new();
        while self.buf.len() >= CHUNK_BYTES {
            let rest = self.buf.split_off(CHUNK_BYTES);
            out.push(std::mem::replace(&mut self.buf, rest));
        }
        out
    }

    pub fn flush(&mut self) -> Option<Vec<u8>> {
        let whole = self.buf.len() - self.buf.len() % 2;
        if whole == 0 {
            self.buf.clear();
            return None;
        }
        let out = self.buf[..whole].to_vec();
        self.buf.clear();
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(channel: u8, pcm: &[u8]) -> Vec<u8> {
        let mut f = vec![channel];
        f.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        f.extend_from_slice(pcm);
        f
    }

    #[test]
    fn reads_frames_until_a_clean_end() {
        let mut stream = frame(0, &[1, 2, 3, 4]);
        stream.extend(frame(1, &[5, 6]));
        let mut r = stream.as_slice();
        assert_eq!(
            read_frame(&mut r).unwrap(),
            Some((Channel::Call, vec![1, 2, 3, 4]))
        );
        assert_eq!(
            read_frame(&mut r).unwrap(),
            Some((Channel::Mic, vec![5, 6]))
        );
        assert_eq!(read_frame(&mut r).unwrap(), None);
    }

    #[test]
    fn rejects_unknown_channels_and_huge_frames() {
        let bad = frame(7, &[0, 0]);
        assert!(read_frame(&mut bad.as_slice()).is_err());
        let mut huge = vec![0u8];
        huge.extend_from_slice(&((MAX_FRAME_BYTES as u32) + 1).to_le_bytes());
        assert!(read_frame(&mut huge.as_slice()).is_err());
        let truncated = &frame(0, &[1, 2, 3, 4])[..7];
        assert!(read_frame(&mut &truncated[..]).is_err());
    }

    #[test]
    fn chunks_at_one_second_and_flushes_whole_samples() {
        let mut c = Chunker::default();
        assert!(c.push(&vec![0u8; CHUNK_BYTES - 1]).is_empty());
        let chunks = c.push(&vec![1u8; CHUNK_BYTES + 4]);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), CHUNK_BYTES);
        assert_eq!(c.flush().map(|b| b.len()), Some(4));
        assert_eq!(c.flush(), None);
        c.push(&[9]);
        assert_eq!(c.flush(), None);
    }

    #[test]
    fn channel_names_match_hive() {
        assert_eq!(Channel::Call.as_str(), "call");
        assert_eq!(Channel::Mic.as_str(), "mic");
        assert_eq!(Channel::from_byte(2), None);
    }
}
