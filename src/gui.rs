//! Desktop GUI (egui/eframe): runs on Windows, macOS and Linux.

use std::net::SocketAddr;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Layout, RichText};

use crate::audio;
use crate::codec::CodecKind;
use crate::engine::{self, Link, LinkConfig, DEFAULT_PORT, FRAME_SIZES_MS};
use crate::jitter::Stats;
use crate::Res;

const GREEN: Color32 = Color32::from_rgb(60, 190, 90);
const AMBER: Color32 = Color32::from_rgb(230, 170, 40);
const RED: Color32 = Color32::from_rgb(220, 60, 50);
const GREY: Color32 = Color32::from_rgb(130, 130, 130);

pub fn run() -> Res<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("AirLink")
            .with_app_id("airlink")
            .with_icon(egui::IconData {
                rgba: include_bytes!("../packaging/icons/airlink-64.rgba").to_vec(),
                width: 64,
                height: 64,
            })
            .with_inner_size([480.0, 720.0])
            .with_min_inner_size([420.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("AirLink", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
        .map_err(|e| e.to_string().into())
}

#[derive(Default, Clone, Copy)]
struct Rates {
    rx: u64,
    concealed: u64,
    late: u64,
}

struct App {
    // Settings
    call: bool,
    peer_text: String,
    port: u16,
    codec: CodecKind,
    bitrate: u32,
    frame_ms: f32,
    channels: usize,
    jitter_ms: f32,
    redundancy: usize,
    inputs: Vec<String>,
    outputs: Vec<String>,
    input: Option<String>,
    output: Option<String>,

    // Running state
    link: Option<Link>,
    error: Option<String>,
    meter_send: f32,
    meter_recv: f32,
    last_tick: Instant,
    last_counts: (u64, u64, u64),
    rates: Rates,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let d = LinkConfig::default();
        let mut app = Self {
            call: true,
            peer_text: String::new(),
            port: DEFAULT_PORT,
            codec: d.codec,
            bitrate: d.bitrate,
            frame_ms: d.frame_ms,
            channels: d.channels,
            jitter_ms: d.jitter_ms,
            redundancy: d.redundancy,
            inputs: Vec::new(),
            outputs: Vec::new(),
            input: None,
            output: None,
            link: None,
            error: None,
            meter_send: 0.0,
            meter_recv: 0.0,
            last_tick: Instant::now(),
            last_counts: (0, 0, 0),
            rates: Rates::default(),
        };
        app.refresh_devices();
        app
    }

    fn refresh_devices(&mut self) {
        let host = cpal::default_host();
        self.inputs = audio::device_names(&host, true);
        self.outputs = audio::device_names(&host, false);
        if self.input.as_ref().is_some_and(|n| !self.inputs.contains(n)) {
            self.input = None;
        }
        if self.output.as_ref().is_some_and(|n| !self.outputs.contains(n)) {
            self.output = None;
        }
    }

    fn config(&self) -> Res<LinkConfig> {
        let peer = if self.call { Some(engine::resolve(&self.peer_text)?) } else { None };
        Ok(LinkConfig {
            bind: SocketAddr::from(([0, 0, 0, 0], self.port)),
            peer,
            codec: self.codec,
            bitrate: self.bitrate,
            frame_ms: self.frame_ms,
            channels: self.channels,
            jitter_ms: self.jitter_ms,
            redundancy: self.redundancy,
            input: self.input.clone(),
            output: self.output.clone(),
            buffer: None,
        })
    }

    fn toggle(&mut self) {
        if self.link.take().is_some() {
            return; // dropping the link stops it
        }
        self.error = None;
        match self.config().and_then(|c| Link::start(&c)) {
            Ok(l) => {
                self.last_counts = (0, 0, 0);
                self.rates = Rates::default();
                self.last_tick = Instant::now();
                self.link = Some(l);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Meters every frame, packet rates once a second.
    fn tick(&mut self) {
        let Some(link) = &self.link else {
            self.meter_send *= 0.8;
            self.meter_recv *= 0.8;
            return;
        };
        let s = &link.stats;
        self.meter_send = Stats::take_peak(&s.in_peak).max(self.meter_send * 0.88);
        self.meter_recv = Stats::take_peak(&s.out_peak).max(self.meter_recv * 0.88);
        if self.last_tick.elapsed() >= Duration::from_secs(1) {
            let now = (s.rx_packets.load(Relaxed), s.concealed.load(Relaxed), s.late.load(Relaxed));
            self.rates = Rates {
                rx: now.0 - self.last_counts.0,
                concealed: now.1 - self.last_counts.1,
                late: now.2 - self.last_counts.2,
            };
            self.last_counts = now;
            self.last_tick = Instant::now();
        }
    }

    fn status(&self) -> (String, Color32) {
        match &self.link {
            None => ("Idle".into(), GREY),
            Some(l) => match l.peer() {
                None => ("Waiting for caller…".into(), AMBER),
                Some(p) if self.rates.rx > 0 || l.stats.rx_packets.load(Relaxed) > 0 => {
                    if self.rates.concealed > self.rates.rx / 20 {
                        (format!("Connected · {p} · lossy"), AMBER)
                    } else {
                        (format!("Connected · {p}"), GREEN)
                    }
                }
                Some(p) => (format!("Calling {p}…"), AMBER),
            },
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Mode");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.call, true, "Call");
                ui.radio_value(&mut self.call, false, "Answer");
            });
            ui.end_row();

            if self.call {
                ui.label("Peer address");
                ui.add(
                    egui::TextEdit::singleline(&mut self.peer_text)
                        .hint_text("host or IP[:port]")
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            }

            ui.label("Local UDP port");
            ui.add(egui::DragValue::new(&mut self.port).range(1024..=65535));
            ui.end_row();

            ui.label("Codec");
            let before = self.codec;
            egui::ComboBox::from_id_salt("codec")
                .selected_text(self.codec.name())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.codec, CodecKind::Opus, CodecKind::Opus.name());
                    ui.selectable_value(&mut self.codec, CodecKind::Pcm24, CodecKind::Pcm24.name());
                });
            if self.codec != before {
                self.redundancy = if self.codec == CodecKind::Opus { 1 } else { 0 };
            }
            ui.end_row();

            if self.codec == CodecKind::Opus {
                ui.label("Bitrate");
                ui.add(egui::Slider::new(&mut self.bitrate, 64..=510).suffix(" kb/s"));
                ui.end_row();
            }

            ui.label("Frame size");
            egui::ComboBox::from_id_salt("frame")
                .selected_text(format!("{} ms", self.frame_ms))
                .show_ui(ui, |ui| {
                    for f in FRAME_SIZES_MS {
                        ui.selectable_value(&mut self.frame_ms, f, format!("{f} ms"));
                    }
                });
            ui.end_row();

            ui.label("Channels");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.channels, 2, "Stereo");
                ui.radio_value(&mut self.channels, 1, "Mono");
            });
            ui.end_row();

            ui.label("Jitter buffer");
            ui.add(egui::Slider::new(&mut self.jitter_ms, 0.0..=200.0).suffix(" ms"));
            ui.end_row();

            ui.label("Redundancy");
            egui::ComboBox::from_id_salt("red")
                .selected_text(match self.redundancy {
                    0 => "Off".to_string(),
                    n => format!("{n}× (re-send last {n})"),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.redundancy, 0, "Off");
                    for n in 1..=3 {
                        ui.selectable_value(&mut self.redundancy, n, format!("{n}× (re-send last {n})"));
                    }
                });
            ui.end_row();

            ui.label("Input");
            device_combo(ui, "in", &mut self.input, &self.inputs);
            ui.end_row();

            ui.label("Output");
            device_combo(ui, "out", &mut self.output, &self.outputs);
            ui.end_row();
        });
        if ui.small_button("⟳ Refresh devices").clicked() {
            self.refresh_devices();
        }

        // Live estimate before connecting.
        let mut c = LinkConfig::default();
        c.codec = self.codec;
        c.bitrate = self.bitrate;
        c.frame_ms = self.frame_ms;
        c.channels = self.channels;
        c.redundancy = self.redundancy;
        let est = self.frame_ms + self.codec.lookahead_ms() + self.jitter_ms + 2.0 * self.frame_ms;
        ui.label(
            RichText::new(format!(
                "≈ {:.1} ms one-way + network · {:.0} kb/s each way",
                est,
                c.kbps() * (self.redundancy + 1) as f32
            ))
            .color(GREY),
        );
    }
}

fn device_combo(ui: &mut egui::Ui, id: &str, sel: &mut Option<String>, names: &[String]) {
    let text = sel.clone().unwrap_or_else(|| "System default".into());
    egui::ComboBox::from_id_salt(id)
        .selected_text(text)
        .width(260.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(sel, None, "System default");
            for n in names {
                ui.selectable_value(sel, Some(n.clone()), n);
            }
        });
}

fn meter(ui: &mut egui::Ui, label: &str, level: f32) {
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 16.0], egui::Label::new(label));
        let width = (ui.available_width() - 70.0).max(60.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 14.0), egui::Sense::hover());
        let db = if level > 0.000_001 { 20.0 * level.log10() } else { -120.0 };
        let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let mut fill = rect;
        fill.set_width(rect.width() * frac);
        let colour = if db > -3.0 {
            RED
        } else if db > -12.0 {
            AMBER
        } else {
            GREEN
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, Color32::from_gray(35));
        painter.rect_filled(fill, 2.0, colour);
        // -18 dBFS reference line (EBU alignment).
        let x = rect.left() + rect.width() * (42.0 / 60.0);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0, Color32::from_gray(110)),
        );
        ui.label(if db > -100.0 { format!("{db:6.1} dB") } else { "   -∞ dB".into() });
    });
}

fn stat_row(ui: &mut egui::Ui, k: &str, v: String) {
    ui.label(RichText::new(k).color(GREY));
    ui.label(v);
    ui.end_row();
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(33));
        self.tick();
        let running = self.link.is_some();

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(RichText::new("AirLink").strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (text, colour) = self.status();
                        ui.label(RichText::new(text).color(colour).strong());
                        ui.label(RichText::new("●").color(colour));
                    });
                });
                ui.separator();

                ui.add_enabled_ui(!running, |ui| self.settings_ui(ui));
                ui.add_space(10.0);

                let label = if running {
                    "Disconnect"
                } else if self.call {
                    "Call"
                } else {
                    "Answer"
                };
                let button = egui::Button::new(RichText::new(label).size(18.0).strong())
                    .fill(if running { Color32::from_rgb(120, 35, 30) } else { Color32::from_rgb(30, 90, 50) });
                if ui.add_sized([ui.available_width(), 38.0], button).clicked() {
                    self.toggle();
                }

                if let Some(e) = &self.error {
                    ui.add_space(4.0);
                    ui.label(RichText::new(e).color(RED));
                }

                ui.add_space(10.0);
                ui.separator();
                meter(ui, "Send", self.meter_send);
                meter(ui, "Receive", self.meter_recv);
                ui.separator();

                if let Some(l) = &self.link {
                    let s = &l.stats;
                    egui::Grid::new("stats").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                        stat_row(ui, "Latency", format!("≈ {:.1} ms + network", l.info.latency_ms));
                        stat_row(ui, "", l.info.latency_detail.clone());
                        stat_row(
                            ui,
                            "Buffer",
                            format!("{:.1} ms", s.depth_us.load(Relaxed) as f32 / 1000.0),
                        );
                        stat_row(ui, "Packets in", format!("{}/s", self.rates.rx));
                        stat_row(ui, "Concealed", format!("{}/s", self.rates.concealed));
                        stat_row(ui, "Late", format!("{}/s", self.rates.late));
                        stat_row(
                            ui,
                            "Underruns",
                            format!(
                                "{}   drift fixes {}",
                                s.underruns.load(Relaxed),
                                s.drift_adjust.load(Relaxed)
                            ),
                        );
                        stat_row(ui, "Sending", l.info.send.clone());
                        stat_row(ui, "Input", l.info.input.clone());
                        stat_row(ui, "Output", l.info.output.clone());
                        stat_row(ui, "Listening", l.info.local.to_string());
                    });
                    if let Some(w) = &l.info.warning {
                        ui.label(RichText::new(w).color(AMBER));
                    }
                }
            });
        });
    }
}
