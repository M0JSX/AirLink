//! The link engine: soundcards + codec + UDP + jitter buffer, shared by the GUI and the CLI.

use std::collections::VecDeque;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::audio;
use crate::codec::{CodecKind, Enc, SR};
use crate::jitter::{Jitter, Stats, StreamParams};
use crate::proto;
use crate::Res;

pub const DEFAULT_PORT: u16 = 5004;
const MAX_UDP_PAYLOAD: usize = 1472; // 1500 MTU minus IPv4 + UDP headers
pub const FRAME_SIZES_MS: [f32; 4] = [2.5, 5.0, 10.0, 20.0];

#[derive(Clone, Debug)]
pub struct LinkConfig {
    pub bind: SocketAddr,
    /// `None` = answer whoever calls first.
    pub peer: Option<SocketAddr>,
    pub codec: CodecKind,
    pub bitrate: u32,
    pub frame_ms: f32,
    pub channels: usize,
    pub jitter_ms: f32,
    pub redundancy: usize,
    pub input: Option<String>,
    pub output: Option<String>,
    pub buffer: Option<u32>,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
            peer: None,
            codec: CodecKind::Opus,
            bitrate: 256,
            frame_ms: 5.0,
            channels: 2,
            jitter_ms: 20.0,
            redundancy: 1,
            input: None,
            output: None,
            buffer: None,
        }
    }
}

impl LinkConfig {
    pub fn frame(&self) -> usize {
        (self.frame_ms * SR as f32 / 1000.0).round() as usize
    }

    pub fn validate(&self) -> Res<()> {
        if !FRAME_SIZES_MS.contains(&self.frame_ms) {
            return Err("frame size must be 2.5, 5, 10 or 20 ms".into());
        }
        if !(1..=2).contains(&self.channels) {
            return Err("channels must be 1 or 2".into());
        }
        if !(6..=510).contains(&self.bitrate) {
            return Err("bitrate must be 6-510 kb/s".into());
        }
        if self.redundancy > 3 {
            return Err("redundancy must be 0-3".into());
        }
        if !(0.0..=1000.0).contains(&self.jitter_ms) {
            return Err("jitter buffer must be 0-1000 ms".into());
        }
        Ok(())
    }

    /// Payload rate of one copy of the stream, kb/s.
    pub fn kbps(&self) -> f32 {
        match self.codec {
            CodecKind::Opus => self.bitrate as f32,
            CodecKind::Pcm24 => (SR as usize * self.channels * 24) as f32 / 1000.0,
        }
    }

    pub fn packet_bytes(&self) -> usize {
        let payload = match self.codec {
            CodecKind::Pcm24 => self.frame() * self.channels * 3,
            CodecKind::Opus => (self.bitrate as usize * 1000 / 8) * self.frame() / SR as usize,
        };
        proto::HEADER_LEN + (self.redundancy + 1) * (2 + payload)
    }
}

/// Resolve "host", "host:port", "1.2.3.4" or "[::1]:5004".
pub fn resolve(s: &str) -> Res<SocketAddr> {
    let s = s.trim();
    if s.is_empty() {
        return Err("enter the address of the other end".into());
    }
    if let Ok(mut it) = s.to_socket_addrs() {
        if let Some(a) = it.next() {
            return Ok(a);
        }
    }
    format!("{s}:{DEFAULT_PORT}")
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve '{s}': {e}"))?
        .next()
        .ok_or_else(|| format!("cannot resolve '{s}'").into())
}

pub struct LinkInfo {
    pub send: String,
    pub input: String,
    pub output: String,
    pub latency_ms: f32,
    pub latency_detail: String,
    pub local: SocketAddr,
    pub warning: Option<String>,
}

/// A running link. Dropping it stops everything.
pub struct Link {
    in_stream: Option<cpal::Stream>,
    out_stream: Option<cpal::Stream>,
    threads: Vec<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    pub stats: Arc<Stats>,
    peer: Arc<Mutex<Option<SocketAddr>>>,
    pub info: LinkInfo,
}

impl Link {
    pub fn peer(&self) -> Option<SocketAddr> {
        *self.peer.lock().unwrap()
    }

    pub fn start(cfg: &LinkConfig) -> Res<Link> {
        cfg.validate()?;
        let frame = cfg.frame();
        let ch = cfg.channels;
        let redundancy = cfg.redundancy;
        let params = StreamParams { codec: cfg.codec, channels: ch, frame };

        let warning = (cfg.packet_bytes() > MAX_UDP_PAYLOAD).then(|| {
            format!(
                "Packets are {} bytes and will be IP-fragmented. Use a smaller frame or less redundancy.",
                cfg.packet_bytes()
            )
        });

        let stats = Arc::new(Stats::default());
        let stop = Arc::new(AtomicBool::new(false));
        let jitter = Arc::new(Mutex::new(Jitter::new(cfg.jitter_ms, stats.clone())));
        let peer = Arc::new(Mutex::new(cfg.peer));

        // A previous link may still be releasing the port for a moment.
        let mut sock = UdpSocket::bind(cfg.bind);
        for _ in 0..10 {
            if sock.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            sock = UdpSocket::bind(cfg.bind);
        }
        let sock = sock.map_err(|e| format!("cannot listen on {}: {e}", cfg.bind))?;
        sock.set_read_timeout(Some(Duration::from_millis(100)))?;
        let local = sock.local_addr()?;

        // Audio.
        let host = cpal::default_host();
        let buffer = cfg.buffer.unwrap_or(frame as u32);
        let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(64);
        let in_dev = audio::find_device(&host, cfg.input.as_deref(), true)?;
        let out_dev = audio::find_device(&host, cfg.output.as_deref(), false)?;
        let (in_stream, in_cfg, in_fmt) =
            audio::start_input(&in_dev, ch, frame, buffer, tx, stats.clone())?;
        let (out_stream, out_cfg, out_fmt) =
            audio::start_output(&out_dev, buffer, jitter.clone(), stats.clone())?;

        let mut threads = Vec::new();

        // Encode + send.
        {
            let sock = sock.try_clone()?;
            let peer = peer.clone();
            let stop = stop.clone();
            let mut enc = Enc::new(cfg.codec, ch, cfg.bitrate)?;
            threads.push(std::thread::Builder::new().name("airlink-tx".into()).spawn(move || {
                let mut hist: VecDeque<Vec<u8>> = VecDeque::with_capacity(redundancy + 1);
                let mut payload = Vec::with_capacity(4096);
                let mut pkt = Vec::with_capacity(8192);
                let mut seq: u64 = 0;
                while !stop.load(Relaxed) {
                    let frame = match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(f) => f,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    if enc.encode(&frame, &mut payload).is_err() {
                        continue;
                    }
                    hist.push_front(payload.clone());
                    hist.truncate(redundancy + 1);
                    proto::build(&mut pkt, params, seq, &hist);
                    let dest = *peer.lock().unwrap();
                    if let Some(d) = dest {
                        let _ = sock.send_to(&pkt, d);
                    }
                    seq += 1;
                }
            })?);
        }

        // Receive -> jitter buffer.
        {
            let peer = peer.clone();
            let jitter = jitter.clone();
            let stats = stats.clone();
            let stop = stop.clone();
            let learn_peer = cfg.peer.is_none();
            threads.push(std::thread::Builder::new().name("airlink-rx".into()).spawn(move || {
                let mut buf = vec![0u8; 65536];
                while !stop.load(Relaxed) {
                    let (n, src) = match sock.recv_from(&mut buf) {
                        Ok(r) => r,
                        Err(_) => continue, // timeout, or ICMP unreachable on Windows
                    };
                    let Some((p, seq, blocks)) = proto::parse(&buf[..n]) else { continue };
                    if learn_peer {
                        let mut pa = peer.lock().unwrap();
                        if *pa != Some(src) {
                            *pa = Some(src);
                        }
                    }
                    stats.rx_packets.fetch_add(1, Relaxed);
                    let mut j = jitter.lock().unwrap();
                    for (i, data) in blocks.iter().enumerate() {
                        if (i as u64) <= seq {
                            j.insert(p, seq - i as u64, data, i == 0);
                        }
                    }
                }
            })?);
        }

        let dev_ms =
            audio::buffer_ms(&in_cfg).unwrap_or(0.0) + audio::buffer_ms(&out_cfg).unwrap_or(0.0);
        let latency_ms = cfg.frame_ms + cfg.codec.lookahead_ms() + cfg.jitter_ms + dev_ms;
        let info = LinkInfo {
            send: format!(
                "{} {:.0} kb/s {}, {} ms frames, redundancy {} (~{:.0} kb/s on the wire)",
                cfg.codec.name(),
                cfg.kbps(),
                if ch == 2 { "stereo" } else { "mono" },
                cfg.frame_ms,
                redundancy,
                cfg.kbps() * (redundancy + 1) as f32
            ),
            input: audio::describe(&in_dev, &in_cfg, in_fmt),
            output: audio::describe(&out_dev, &out_cfg, out_fmt),
            latency_ms,
            latency_detail: format!(
                "frame {} + codec {} + jitter {} + soundcards {:.1} ms",
                cfg.frame_ms,
                cfg.codec.lookahead_ms(),
                cfg.jitter_ms,
                dev_ms
            ),
            local,
            warning,
        };

        Ok(Link {
            in_stream: Some(in_stream),
            out_stream: Some(out_stream),
            threads,
            stop,
            stats,
            peer,
            info,
        })
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        self.in_stream.take();
        self.out_stream.take();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
