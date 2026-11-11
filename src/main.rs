//! AirLink — low-latency, high-fidelity two-way P2P audio link.
//!
//! No arguments: GUI. Any argument (e.g. `--headless`): command-line mode for servers.

// Release builds on Windows are a GUI app with no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod cli;
mod codec;
mod engine;
mod gui;
mod jitter;
mod proto;

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = if args.is_empty() { gui::run() } else { cli::run(args) };
    if let Err(e) = result {
        eprintln!("airlink: {e}");
        std::process::exit(1);
    }
}
