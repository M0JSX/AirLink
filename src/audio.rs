//! Soundcard I/O via cpal (CoreAudio on macOS, WASAPI on Windows, ALSA on Linux).

use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, Device, FromSample, Host, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    SupportedBufferSize,
};

use crate::codec::SR;
use crate::jitter::{Jitter, Player, Stats};
use crate::Res;

pub fn device_names(host: &Host, input: bool) -> Vec<String> {
    let devs: Vec<Device> = match if input {
        host.input_devices().map(|d| d.collect())
    } else {
        host.output_devices().map(|d| d.collect())
    } {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    devs.iter().filter_map(|d| d.name().ok()).collect()
}

/// Default device, or the first whose name contains `name` (case-insensitive).
pub fn find_device(host: &Host, name: Option<&str>, input: bool) -> Res<Device> {
    let kind = if input { "input" } else { "output" };
    match name {
        None => (if input { host.default_input_device() } else { host.default_output_device() })
            .ok_or_else(|| format!("no default {kind} device").into()),
        Some(n) => {
            let n = n.to_lowercase();
            let devs: Vec<Device> = if input {
                host.input_devices()?.collect()
            } else {
                host.output_devices()?.collect()
            };
            let name_of = |d: &Device| d.name().map(|x| x.to_lowercase()).unwrap_or_default();
            let exact = devs.iter().position(|d| name_of(d) == n);
            let idx = exact.or_else(|| devs.iter().position(|d| name_of(d).contains(&n)));
            idx.map(|i| devs.into_iter().nth(i).unwrap())
                .ok_or_else(|| format!("no {kind} device matching '{n}' (try --list)").into())
        }
    }
}

/// Pick a 48 kHz config, preferring float samples and the fewest channels that cover `want_ch`.
fn pick_config(
    dev: &Device,
    input: bool,
    want_ch: usize,
    buffer: u32,
) -> Res<(StreamConfig, SampleFormat)> {
    let ranges: Vec<cpal::SupportedStreamConfigRange> = if input {
        dev.supported_input_configs()?.collect()
    } else {
        dev.supported_output_configs()?.collect()
    };
    let rank = |f: SampleFormat| match f {
        SampleFormat::F32 => 0,
        SampleFormat::I32 => 1,
        SampleFormat::I16 => 2,
        _ => 9,
    };
    let best = ranges
        .into_iter()
        .filter(|r| {
            r.min_sample_rate().0 <= SR && r.max_sample_rate().0 >= SR && rank(r.sample_format()) < 9
        })
        .min_by_key(|r| (rank(r.sample_format()), (r.channels() as usize) < want_ch, r.channels()))
        .ok_or("device does not support 48 kHz")?;
    let fmt = best.sample_format();
    let sup = best.with_sample_rate(cpal::SampleRate(SR));
    let mut cfg: StreamConfig = sup.config();
    cfg.buffer_size = match sup.buffer_size() {
        SupportedBufferSize::Range { min, max } if min <= max => {
            BufferSize::Fixed(buffer.clamp(*min, *max))
        }
        _ => BufferSize::Default,
    };
    Ok((cfg, fmt))
}

pub fn describe(dev: &Device, cfg: &StreamConfig, fmt: SampleFormat) -> String {
    let buf = match cfg.buffer_size {
        BufferSize::Fixed(n) => format!("{n} samples ({:.1} ms)", n as f32 * 1000.0 / SR as f32),
        BufferSize::Default => "default".into(),
    };
    format!(
        "{} — {} ch, {:?}, buffer {}",
        dev.name().unwrap_or_else(|_| "?".into()),
        cfg.channels,
        fmt,
        buf
    )
}

pub fn buffer_ms(cfg: &StreamConfig) -> Option<f32> {
    match cfg.buffer_size {
        BufferSize::Fixed(n) => Some(n as f32 * 1000.0 / SR as f32),
        BufferSize::Default => None,
    }
}

/// Capture: collects `frame` samples per channel and hands each complete frame to the encoder.
pub fn start_input(
    dev: &Device,
    link_ch: usize,
    frame: usize,
    buffer: u32,
    tx: SyncSender<Vec<f32>>,
    stats: Arc<Stats>,
) -> Res<(Stream, StreamConfig, SampleFormat)> {
    let (cfg, fmt) = pick_config(dev, true, link_ch, buffer)?;
    let stream = match fmt {
        SampleFormat::F32 => build_input::<f32>(dev, &cfg, link_ch, frame, tx, stats)?,
        SampleFormat::I32 => build_input::<i32>(dev, &cfg, link_ch, frame, tx, stats)?,
        SampleFormat::I16 => build_input::<i16>(dev, &cfg, link_ch, frame, tx, stats)?,
        f => return Err(format!("unsupported input sample format {f:?}").into()),
    };
    stream.play()?;
    Ok((stream, cfg, fmt))
}

fn build_input<T>(
    dev: &Device,
    cfg: &StreamConfig,
    link_ch: usize,
    frame: usize,
    tx: SyncSender<Vec<f32>>,
    stats: Arc<Stats>,
) -> Res<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let in_ch = cfg.channels as usize;
    let want = frame * link_ch;
    let mut acc: Vec<f32> = Vec::with_capacity(want);
    let stream = dev.build_input_stream(
        cfg,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mut peak = 0.0f32;
            for f in data.chunks(in_ch) {
                for c in 0..link_ch {
                    // Mono device feeding a stereo link: duplicate channel 1.
                    let v = f32::from_sample(f[if c < f.len() { c } else { 0 }]);
                    peak = peak.max(v.abs());
                    acc.push(v);
                }
                if acc.len() >= want {
                    let full = std::mem::replace(&mut acc, Vec::with_capacity(want));
                    let _ = tx.try_send(full); // never block the audio thread
                }
            }
            Stats::bump_peak(&stats.in_peak, peak);
        },
        |e| eprintln!("\ninput stream error: {e}"),
        None,
    )?;
    Ok(stream)
}

/// Playout: pulls from the jitter buffer via a `Player`.
pub fn start_output(
    dev: &Device,
    buffer: u32,
    jitter: Arc<Mutex<Jitter>>,
    stats: Arc<Stats>,
) -> Res<(Stream, StreamConfig, SampleFormat)> {
    let (cfg, fmt) = pick_config(dev, false, 2, buffer)?;
    let stream = match fmt {
        SampleFormat::F32 => build_output::<f32>(dev, &cfg, jitter, stats)?,
        SampleFormat::I32 => build_output::<i32>(dev, &cfg, jitter, stats)?,
        SampleFormat::I16 => build_output::<i16>(dev, &cfg, jitter, stats)?,
        f => return Err(format!("unsupported output sample format {f:?}").into()),
    };
    stream.play()?;
    Ok((stream, cfg, fmt))
}

fn build_output<T>(
    dev: &Device,
    cfg: &StreamConfig,
    jitter: Arc<Mutex<Jitter>>,
    stats: Arc<Stats>,
) -> Res<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let out_ch = cfg.channels as usize;
    let mut player = Player::new(jitter);
    let stream = dev.build_output_stream(
        cfg,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let mut peak = 0.0f32;
            for f in data.chunks_mut(out_ch) {
                let (l, r) = player.next();
                peak = peak.max(l.abs()).max(r.abs());
                if out_ch == 1 {
                    f[0] = T::from_sample((l + r) * 0.5);
                } else {
                    for (c, o) in f.iter_mut().enumerate() {
                        *o = T::from_sample(match c {
                            0 => l,
                            1 => r,
                            _ => 0.0,
                        });
                    }
                }
            }
            Stats::bump_peak(&stats.out_peak, peak);
        },
        |e| eprintln!("\noutput stream error: {e}"),
        None,
    )?;
    Ok(stream)
}
