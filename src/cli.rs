//! Headless mode, for studio servers and racks: `airlink --headless [options]`.

use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use crate::codec::CodecKind;
use crate::engine::{self, Link, LinkConfig};
use crate::{audio, Res};

const USAGE: &str = "\
AirLink — two-way P2P audio link

Run with no arguments for the GUI. Any option runs headless.

USAGE:
  airlink --headless [OPTIONS]

  Studio end:  airlink --headless
  Remote end:  airlink --peer studio.example.com

OPTIONS:
  -p, --peer HOST[:PORT]   Address of the other end. Omit to answer whoever calls.
  -b, --bind ADDR:PORT     Local address to listen on        [default: 0.0.0.0:5004]
  -c, --codec opus|pcm     opus = Opus CELT low-delay, pcm = 24-bit linear  [default: opus]
      --bitrate KBPS       Opus bitrate, 6-510                [default: 256]
  -f, --frame MS           Frame size: 2.5, 5, 10 or 20       [default: 5]
      --channels 1|2       Mono or stereo                     [default: 2]
  -j, --jitter MS          Jitter buffer target               [default: 20]
  -r, --redundancy N       Re-send previous N frames in each packet, 0-3
                                                [default: 1 for opus, 0 for pcm]
  -i, --input NAME         Input device (name or part of it)  [default: system default]
  -o, --output NAME        Output device (name or part of it) [default: system default]
      --buffer SAMPLES     Soundcard buffer size              [default: one frame]
  -l, --list               List audio devices and exit
      --headless           Run without the GUI using defaults
  -h, --help               Show this help
";

fn val(it: &mut impl Iterator<Item = String>, name: &str) -> Res<String> {
    it.next().ok_or_else(|| format!("{name} needs a value").into())
}

pub fn run(args: Vec<String>) -> Res<()> {
    let mut c = LinkConfig::default();
    let mut redundancy = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "--peer" => c.peer = Some(engine::resolve(&val(&mut it, &a)?)?),
            "-b" | "--bind" => c.bind = engine::resolve(&val(&mut it, &a)?)?,
            "-c" | "--codec" => {
                c.codec = match val(&mut it, &a)?.to_lowercase().as_str() {
                    "opus" => CodecKind::Opus,
                    "pcm" | "l24" => CodecKind::Pcm24,
                    other => return Err(format!("unknown codec '{other}' (opus|pcm)").into()),
                }
            }
            "--bitrate" => c.bitrate = val(&mut it, &a)?.parse()?,
            "-f" | "--frame" => c.frame_ms = val(&mut it, &a)?.parse()?,
            "--channels" => c.channels = val(&mut it, &a)?.parse()?,
            "-j" | "--jitter" => c.jitter_ms = val(&mut it, &a)?.parse()?,
            "-r" | "--redundancy" => redundancy = Some(val(&mut it, &a)?.parse()?),
            "-i" | "--input" => c.input = Some(val(&mut it, &a)?),
            "-o" | "--output" => c.output = Some(val(&mut it, &a)?),
            "--buffer" => c.buffer = Some(val(&mut it, &a)?.parse()?),
            "--headless" => {}
            "-l" | "--list" => {
                let host = cpal::default_host();
                println!("Input devices:");
                for n in audio::device_names(&host, true) {
                    println!("  {n}");
                }
                println!("Output devices:");
                for n in audio::device_names(&host, false) {
                    println!("  {n}");
                }
                return Ok(());
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("unknown option '{other}' (see --help)").into()),
        }
    }
    c.redundancy = redundancy.unwrap_or(if c.codec == CodecKind::Opus { 1 } else { 0 });

    let link = Link::start(&c)?;
    let i = &link.info;
    println!("AirLink v{} (headless)", env!("CARGO_PKG_VERSION"));
    println!("  Send     {}", i.send);
    println!("  Input    {}", i.input);
    println!("  Output   {}", i.output);
    println!("  Latency  ≈ {:.1} ms one-way + network ({})", i.latency_ms, i.latency_detail);
    println!("  Listening on {}", i.local);
    match c.peer {
        Some(p) => println!("  Calling {p}"),
        None => println!("  Waiting for the other end to call…"),
    }
    if let Some(w) = &i.warning {
        println!("  Warning: {w}");
    }
    println!();

    let s = &link.stats;
    let mut last = (0u64, 0u64, 0u64);
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let now = (s.rx_packets.load(Relaxed), s.concealed.load(Relaxed), s.late.load(Relaxed));
        let peer = link.peer().map(|a| a.to_string()).unwrap_or_else(|| "—".into());
        eprint!(
            "\r{}  rx {:4} pkt/s  concealed {:3}  late {:3}  buffer {:5.1} ms  underruns {}  drift-fix {}     ",
            peer,
            now.0 - last.0,
            now.1 - last.1,
            now.2 - last.2,
            s.depth_us.load(Relaxed) as f32 / 1000.0,
            s.underruns.load(Relaxed),
            s.drift_adjust.load(Relaxed),
        );
        last = now;
    }
}
