//! Wire format (UDP, one packet per frame):
//!
//! ```text
//! 0..2   magic "AL"
//! 2      version (1)
//! 3      codec (0 = PCM24, 1 = Opus)
//! 4      channels (1 or 2)
//! 5      block count (1 + redundancy)
//! 6..8   frame size in samples per channel (u16 BE)
//! 8..16  sequence number of block 0 (u64 BE)
//! then per block: u16 BE length + payload. Block i carries frame (seq - i),
//! so each packet also re-sends the previous N frames as loss protection.
//! ```

use std::collections::VecDeque;

use crate::codec::CodecKind;
use crate::jitter::StreamParams;

const MAGIC: [u8; 2] = *b"AL";
const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 16;
pub const VALID_FRAMES: [usize; 4] = [120, 240, 480, 960];

pub fn build(out: &mut Vec<u8>, p: StreamParams, seq: u64, blocks: &VecDeque<Vec<u8>>) {
    out.clear();
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(p.codec as u8);
    out.push(p.channels as u8);
    out.push(blocks.len() as u8);
    out.extend_from_slice(&(p.frame as u16).to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    for b in blocks {
        out.extend_from_slice(&(b.len() as u16).to_be_bytes());
        out.extend_from_slice(b);
    }
}

/// Returns stream params, the sequence number of block 0, and the blocks (newest first).
pub fn parse(buf: &[u8]) -> Option<(StreamParams, u64, Vec<&[u8]>)> {
    if buf.len() < HEADER_LEN || buf[0..2] != MAGIC[..] || buf[2] != VERSION {
        return None;
    }
    let codec = CodecKind::from_u8(buf[3])?;
    let channels = buf[4] as usize;
    let nblocks = buf[5] as usize;
    let frame = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    if !(1..=2).contains(&channels) || !VALID_FRAMES.contains(&frame) || nblocks == 0 {
        return None;
    }
    let seq = u64::from_be_bytes(buf[8..16].try_into().ok()?);
    let mut blocks = Vec::with_capacity(nblocks);
    let mut pos = HEADER_LEN;
    for _ in 0..nblocks {
        if pos + 2 > buf.len() {
            return None;
        }
        let len = u16::from_be_bytes([buf[pos], buf[pos + 1]]) as usize;
        pos += 2;
        if pos + len > buf.len() {
            return None;
        }
        blocks.push(&buf[pos..pos + len]);
        pos += len;
    }
    Some((StreamParams { codec, channels, frame }, seq, blocks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let p = StreamParams { codec: CodecKind::Opus, channels: 2, frame: 240 };
        let mut hist = VecDeque::new();
        hist.push_back(vec![1, 2, 3]);
        hist.push_back(vec![4, 5]);
        let mut out = Vec::new();
        build(&mut out, p, 42, &hist);
        let (p2, seq, blocks) = parse(&out).unwrap();
        assert_eq!(p, p2);
        assert_eq!(seq, 42);
        assert_eq!(blocks, vec![&[1u8, 2, 3][..], &[4u8, 5][..]]);
        assert!(parse(&out[..out.len() - 1]).is_none());
    }
}
