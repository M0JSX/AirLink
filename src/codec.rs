//! Codecs: Opus in restricted low-delay (CELT-only) mode, and linear 24-bit PCM.

use crate::Res;

/// Everything runs at 48 kHz — Opus's native rate and the broadcast standard.
pub const SR: u32 = 48_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CodecKind {
    /// Uncompressed 24-bit big-endian PCM (L24). Zero codec delay, ~1.15 Mb/s per channel.
    Pcm24 = 0,
    /// Opus, CELT-only low-delay mode. 2.5 ms lookahead, transparent at 256 kb/s stereo.
    Opus = 1,
}

impl CodecKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Pcm24),
            1 => Some(Self::Opus),
            _ => None,
        }
    }

    /// Codec algorithmic delay in ms, on top of the frame size.
    pub fn lookahead_ms(self) -> f32 {
        match self {
            Self::Pcm24 => 0.0,
            Self::Opus => 2.5,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Pcm24 => "PCM 24-bit",
            Self::Opus => "Opus (CELT low-delay)",
        }
    }
}

fn opus_channels(ch: usize) -> opus::Channels {
    if ch == 1 {
        opus::Channels::Mono
    } else {
        opus::Channels::Stereo
    }
}

pub enum Enc {
    Pcm,
    Opus(opus::Encoder),
}

impl Enc {
    pub fn new(kind: CodecKind, channels: usize, bitrate_kbps: u32) -> Res<Self> {
        Ok(match kind {
            CodecKind::Pcm24 => Enc::Pcm,
            CodecKind::Opus => {
                // Application::LowDelay == OPUS_APPLICATION_RESTRICTED_LOWDELAY:
                // CELT only, no SILK/hybrid, minimum possible algorithmic delay.
                let mut e =
                    opus::Encoder::new(SR, opus_channels(channels), opus::Application::LowDelay)?;
                e.set_bitrate(opus::Bitrate::Bits((bitrate_kbps * 1000) as i32))?;
                // Constant bitrate: predictable packet sizes, best for contribution links.
                e.set_vbr(false)?;
                Enc::Opus(e)
            }
        })
    }

    /// Encode one interleaved frame into `out`.
    pub fn encode(&mut self, pcm: &[f32], out: &mut Vec<u8>) -> Res<()> {
        out.clear();
        match self {
            Enc::Pcm => {
                out.reserve(pcm.len() * 3);
                for &s in pcm {
                    let v = (s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                    out.extend_from_slice(&v.to_be_bytes()[1..4]);
                }
            }
            Enc::Opus(e) => {
                out.resize(4000, 0);
                let n = e.encode_float(pcm, out)?;
                out.truncate(n);
            }
        }
        Ok(())
    }
}

pub enum Dec {
    Pcm,
    Opus(opus::Decoder),
}

impl Dec {
    pub fn new(kind: CodecKind, channels: usize) -> Res<Self> {
        Ok(match kind {
            CodecKind::Pcm24 => Dec::Pcm,
            CodecKind::Opus => Dec::Opus(opus::Decoder::new(SR, opus_channels(channels))?),
        })
    }

    /// Decode one frame into `out` (interleaved, exactly one frame long).
    /// `None` means the packet was lost: Opus runs packet-loss concealment, PCM outputs silence.
    pub fn decode(&mut self, data: Option<&[u8]>, out: &mut [f32], channels: usize) {
        match self {
            Dec::Pcm => match data {
                Some(d) => {
                    for (i, o) in out.iter_mut().enumerate() {
                        let p = i * 3;
                        *o = if p + 2 < d.len() {
                            (i32::from_be_bytes([d[p], d[p + 1], d[p + 2], 0]) >> 8) as f32
                                / 8_388_608.0
                        } else {
                            0.0
                        };
                    }
                }
                None => out.fill(0.0),
            },
            Dec::Opus(dec) => {
                // An empty slice tells libopus to conceal (PLC) for out.len()/channels samples.
                let input: &[u8] = data.unwrap_or(&[]);
                match dec.decode_float(input, out, false) {
                    Ok(n) => {
                        let got = n * channels;
                        if got < out.len() {
                            out[got..].fill(0.0);
                        }
                    }
                    Err(_) => out.fill(0.0),
                }
            }
        }
    }
}
