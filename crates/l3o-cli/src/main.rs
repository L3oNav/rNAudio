//! `l3o` — command-line front end and M0 smoke tests.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "l3o",
    version,
    about = "L3O Audio — mixer, DSP and Streamer Mode routing",
    long_about = None
)]
struct Cli {
    /// Enable debug logging.
    #[arg(long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List output and capture devices.
    Devices,
    /// List processes that currently own an audio session.
    Processes,
    /// Print the capture backend capability snapshot.
    Caps,
    /// Play a test tone on an output device.
    Tone {
        /// Frequency in Hz.
        #[arg(long, default_value_t = 1000.0)]
        hz: f32,
        /// Duration in seconds.
        #[arg(long, default_value_t = 2.0)]
        secs: f32,
        /// Exact device name (see `l3o devices`).
        #[arg(long)]
        device: Option<String>,
    },
    /// Show output device format and estimated latency.
    Latency {
        /// Exact device name (see `l3o devices`).
        #[arg(long)]
        device: Option<String>,
    },
    /// Show the detected platform backend.
    Status,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let filter = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .init();

    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Option<Command>) -> Result<(), Box<dyn std::error::Error>> {
    match command.unwrap_or(Command::Status) {
        Command::Devices => list_devices(),
        Command::Processes => list_processes(),
        Command::Caps => {
            println!("{}", l3o_io::capabilities_report());
            Ok(())
        }
        Command::Tone { hz, secs, device } => {
            println!("playing {hz} Hz for {secs} s ...");
            l3o_io::play_tone(hz, secs, device.as_deref())?;
            println!("done");
            Ok(())
        }
        Command::Latency { device } => show_latency(device.as_deref()),
        Command::Status => {
            let backend = l3o_platform::detect();
            println!("L3O Audio {}", env!("CARGO_PKG_VERSION"));
            println!("platform backend: {}", backend.name());
            println!("capture library:  flexaudio {}", flexaudio_version());
            println!("playback library: cpal (see Cargo.lock)");
            println!();
            println!("M0 smoke tests: l3o devices | processes | caps | tone | latency");
            Ok(())
        }
    }
}

fn list_devices() -> Result<(), Box<dyn std::error::Error>> {
    let outputs = l3o_io::list_output_devices()?;
    println!("Output devices ({}):", outputs.len());
    for device in &outputs {
        let default = if device.is_default { " [default]" } else { "" };
        println!(
            "  {}{} — {} ch, {} Hz, {}, buffer {}..{}",
            device.name,
            default,
            device.channels,
            device.sample_rate,
            device.sample_format,
            device
                .min_buffer_frames
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".to_string()),
            device
                .max_buffer_frames
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".to_string()),
        );
    }

    let captures = l3o_io::list_capture_devices()?;
    println!();
    println!("Capture devices ({}):", captures.len());
    for device in &captures {
        let default = if device.is_default { " [default]" } else { "" };
        println!(
            "  {}{} — {} ({}){}",
            device.name,
            default,
            device.id,
            device.kind,
            if device.format_available {
                format!(", {} ch, {} Hz", device.channels, device.sample_rate)
            } else {
                String::new()
            },
        );
    }
    Ok(())
}

fn list_processes() -> Result<(), Box<dyn std::error::Error>> {
    let processes = l3o_io::list_processes()?;
    println!("Audio processes ({}):", processes.len());
    for process in &processes {
        let state = match process.is_output_active {
            Some(true) => "playing",
            Some(false) => "idle",
            None => "unknown",
        };
        let executable = process.executable.as_deref().unwrap_or("?");
        println!(
            "  {:>6}  {:<28} {:<24} {}",
            process.pid, process.name, executable, state
        );
    }
    Ok(())
}

fn show_latency(device: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let summary = l3o_io::describe_output(device)?;
    println!("device:        {}", summary.name);
    println!("channels:      {}", summary.channels);
    println!("sample rate:   {} Hz", summary.sample_rate);
    println!("sample format: {}", summary.sample_format);
    match (summary.min_buffer_frames, summary.max_buffer_frames) {
        (Some(min), Some(max)) => println!("buffer range:  {min}..{max} frames"),
        _ => println!("buffer range:  unknown"),
    }
    match summary.estimated_latency_ms {
        Some(ms) => println!("min latency:   {ms:.2} ms (device buffer only)"),
        None => println!("min latency:   unknown"),
    }
    Ok(())
}

fn flexaudio_version() -> &'static str {
    // flexaudio does not expose a version constant; keep the pinned version visible.
    "0.8.1"
}
