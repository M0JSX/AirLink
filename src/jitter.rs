//! Adaptive jitter buffer and the playout side of the link.
//!
//! The network thread inserts encoded frames by sequence number; the audio output
//! callback pulls one frame at a time. Missing frames are concealed (Opus PLC).
//! Clock drift between the two soundcards is handled by occasionally dropping a frame
//! (buffer persistently too deep) or inserting a concealed one (persistently too shallow).

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use crate::codec::{CodecKind, Dec, SR};

#[derive(Default)]
pub struct Stats {
    pub rx_packets: AtomicU64,
    pub concealed: AtomicU64,
    pub late: AtomicU64,
    pub underruns: AtomicU64,
    pub drift_adjust: AtomicU64,
    pub depth_us: AtomicU64,
    /// Peak levels (f32 bits) since the last read, for meters.
    pub in_peak: AtomicU32,
    pub out_peak: AtomicU32,
}

impl Stats {
    pub fn bump_peak(a: &AtomicU32, v: f32) {
        if v > f32::from_bits(a.load(Relaxed)) {
            a.store(v.to_bits(), Relaxed);
        }
    }

    pub fn take_peak(a: &AtomicU32) -> f32 {
        f32::from_bits(a.swap(0, Relaxed))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StreamParams {
    pub codec: CodecKind,
    pub channels: usize,
    /// Samples per channel per frame.
    pub frame: usize,
}

impl StreamParams {
    pub fn frame_us(&self) -> u64 {
        self.frame as u64 * 1_000_000 / SR as u64
    }
}

pub enum Slot {
    Frame(Vec<u8>),
    Lost,
    Silence,
}

/// Number of consecutive pops the buffer must sit outside its target band
/// before a drift correction (drop or insert one frame) is applied.
const DRIFT_PATIENCE: usize = 200;

pub struct Jitter {
    pkts: BTreeMap<u64, Vec<u8>>,
    params: Option<StreamParams>,
    next: Option<u64>,
    target_us: u64,
    target: usize,
    high_run: usize,
    low_run: usize,
    empty_run: usize,
    stats: Arc<Stats>,
}

impl Jitter {
    pub fn new(target_ms: f32, stats: Arc<Stats>) -> Self {
        Self {
            pkts: BTreeMap::new(),
            params: None,
            next: None,
            target_us: (target_ms * 1000.0) as u64,
            target: 1,
            high_run: 0,
            low_run: 0,
            empty_run: 0,
            stats,
        }
    }

    fn reset(&mut self) {
        self.pkts.clear();
        self.next = None;
        self.high_run = 0;
        self.low_run = 0;
        self.empty_run = 0;
    }

    fn max_frames(&self) -> usize {
        self.target * 4 + 50
    }

    pub fn insert(&mut self, p: StreamParams, seq: u64, data: &[u8], primary: bool) {
        if self.params != Some(p) {
            // New peer or the peer changed codec/format: start clean.
            self.params = Some(p);
            self.reset();
            let fu = p.frame_us();
            self.target = (self.target_us.div_ceil(fu) as usize).max(1);
        }
        if let Some(n) = self.next {
            if seq < n {
                if primary && n - seq > 1000 {
                    // Sequence went far backwards: the peer restarted.
                    self.reset();
                } else {
                    if primary {
                        self.stats.late.fetch_add(1, Relaxed);
                    }
                    return; // already played (or concealed) — redundant copies land here too
                }
            }
        }
        self.pkts.entry(seq).or_insert_with(|| data.to_vec());
        while self.pkts.len() > self.max_frames() {
            if let Some((k, _)) = self.pkts.pop_first() {
                if let Some(n) = self.next {
                    if k >= n {
                        self.next = Some(k + 1);
                    }
                }
            }
        }
    }

    fn span(&self, n: u64) -> usize {
        match self.pkts.keys().next_back() {
            Some(&last) if last >= n => (last - n + 1) as usize,
            _ => 0,
        }
    }

    pub fn pop(&mut self) -> (Option<StreamParams>, Slot) {
        let Some(p) = self.params else {
            return (None, Slot::Silence);
        };
        let fu = p.frame_us();

        let mut n = match self.next {
            Some(n) => n,
            None => {
                // Pre-buffering: wait until the target depth is reached.
                if self.pkts.len() >= self.target {
                    *self.pkts.keys().next().unwrap()
                } else {
                    self.stats.depth_us.store(self.pkts.len() as u64 * fu, Relaxed);
                    return (Some(p), Slot::Silence);
                }
            }
        };

        // Jump over a huge gap rather than concealing for seconds.
        if let Some(&first) = self.pkts.keys().next() {
            if first > n + self.max_frames() as u64 {
                n = first;
            }
        }

        // Drift control.
        let span = self.span(n);
        let high = self.target + (self.target / 2).max(2);
        let low = self.target / 2;
        self.high_run = if span > high { self.high_run + 1 } else { 0 };
        self.low_run = if span < low && span > 0 { self.low_run + 1 } else { 0 };

        if self.high_run > DRIFT_PATIENCE {
            // Persistently too deep: drop one frame to pull latency back down.
            self.pkts.remove(&n);
            n += 1;
            self.high_run = 0;
            self.stats.drift_adjust.fetch_add(1, Relaxed);
        } else if self.low_run > DRIFT_PATIENCE {
            // Persistently too shallow: play one concealed frame without consuming any.
            self.low_run = 0;
            self.next = Some(n);
            self.stats.drift_adjust.fetch_add(1, Relaxed);
            return (Some(p), Slot::Lost);
        }

        self.next = Some(n + 1);
        let slot = match self.pkts.remove(&n) {
            Some(d) => {
                self.empty_run = 0;
                Slot::Frame(d)
            }
            None => {
                self.stats.concealed.fetch_add(1, Relaxed);
                if self.pkts.is_empty() {
                    self.empty_run += 1;
                    if self.empty_run == 1 {
                        self.stats.underruns.fetch_add(1, Relaxed);
                    }
                    // Nothing arriving for 200 ms: stop concealing and re-buffer.
                    if self.empty_run as u64 * fu > 200_000 {
                        self.reset();
                    }
                }
                Slot::Lost
            }
        };
        let depth = self.next.map(|n| self.span(n)).unwrap_or(0);
        self.stats.depth_us.store(depth as u64 * fu, Relaxed);
        (Some(p), slot)
    }
}

/// Lives inside the audio output callback: pulls frames from the jitter buffer,
/// decodes them and hands out one stereo sample pair at a time.
pub struct Player {
    jitter: Arc<Mutex<Jitter>>,
    dec: Option<(StreamParams, Dec)>,
    fifo: VecDeque<f32>,
    buf: Vec<f32>,
    ch: usize,
}

impl Player {
    pub fn new(jitter: Arc<Mutex<Jitter>>) -> Self {
        Self {
            jitter,
            dec: None,
            fifo: VecDeque::with_capacity(8192),
            buf: Vec::with_capacity(4096),
            ch: 2,
        }
    }

    /// Next output sample as (left, right).
    pub fn next(&mut self) -> (f32, f32) {
        if self.fifo.len() < self.ch {
            self.refill();
        }
        if self.ch == 1 {
            let s = self.fifo.pop_front().unwrap_or(0.0);
            (s, s)
        } else {
            let l = self.fifo.pop_front().unwrap_or(0.0);
            let r = self.fifo.pop_front().unwrap_or(0.0);
            (l, r)
        }
    }

    fn refill(&mut self) {
        let (params, slot) = match self.jitter.lock() {
            Ok(mut j) => j.pop(),
            Err(_) => (None, Slot::Silence),
        };
        let Some(p) = params else {
            self.fifo.extend(std::iter::repeat(0.0).take(240 * self.ch));
            return;
        };
        if self.dec.as_ref().map(|(dp, _)| *dp != p).unwrap_or(true) {
            match Dec::new(p.codec, p.channels) {
                Ok(d) => self.dec = Some((p, d)),
                Err(_) => {
                    self.fifo.extend(std::iter::repeat(0.0).take(240 * self.ch));
                    return;
                }
            }
        }
        self.ch = p.channels;
        self.buf.resize(p.frame * p.channels, 0.0);
        let dec = &mut self.dec.as_mut().unwrap().1;
        match slot {
            Slot::Frame(d) => dec.decode(Some(&d), &mut self.buf, p.channels),
            Slot::Lost => dec.decode(None, &mut self.buf, p.channels),
            Slot::Silence => self.buf.fill(0.0),
        }
        self.fifo.extend(self.buf.iter().copied());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> StreamParams {
        StreamParams { codec: CodecKind::Pcm24, channels: 2, frame: 240 }
    }

    #[test]
    fn buffers_then_plays_in_order_and_conceals_gaps() {
        let stats = Arc::new(Stats::default());
        let mut j = Jitter::new(15.0, stats.clone()); // 3 frames of 5 ms
        j.insert(params(), 10, &[1], true);
        assert!(matches!(j.pop().1, Slot::Silence));
        j.insert(params(), 12, &[3], true);
        j.insert(params(), 11, &[2], true);
        assert!(matches!(j.pop().1, Slot::Frame(ref d) if d == &[1]));
        assert!(matches!(j.pop().1, Slot::Frame(ref d) if d == &[2]));
        assert!(matches!(j.pop().1, Slot::Frame(ref d) if d == &[3]));
        j.insert(params(), 15, &[6], true);
        assert!(matches!(j.pop().1, Slot::Lost)); // 13 missing
        j.insert(params(), 13, &[4], true); // too late now
        assert_eq!(stats.late.load(Relaxed), 1);
        assert!(matches!(j.pop().1, Slot::Lost)); // 14
        assert!(matches!(j.pop().1, Slot::Frame(ref d) if d == &[6]));
    }

    #[test]
    fn redundant_copy_fills_hole() {
        let stats = Arc::new(Stats::default());
        let mut j = Jitter::new(5.0, stats);
        j.insert(params(), 0, &[0], true);
        // packet 1 lost; packet 2 carries a redundant copy of 1
        j.insert(params(), 2, &[2], true);
        j.insert(params(), 1, &[1], false);
        assert!(matches!(j.pop().1, Slot::Frame(ref d) if d == &[0]));
        assert!(matches!(j.pop().1, Slot::Frame(ref d) if d == &[1]));
        assert!(matches!(j.pop().1, Slot::Frame(ref d) if d == &[2]));
    }
}
