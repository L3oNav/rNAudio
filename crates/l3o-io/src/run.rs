//! Live device wiring: flexaudio capture streams feed the render graph and
//! cpal plays the monitor mix.
//!
//! This is the P1.3 "real Linux" path: it needs actual hardware and is
//! covered by the smoke-test checklist, not unit tests. Nothing here panics
//! on the audio path; failures return `Err` or move a counter.
//!
//! The caller takes the engine's [`ControlSender`] before handing the engine
//! to [`start`]; this module owns no control surface.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use l3o_core::{BusId, CANONICAL_SAMPLE_RATE};
use l3o_engine::Engine;

use crate::graph::RenderStatsSnapshot;

use crate::IoError;
use crate::graph::{CaptureChunk, RenderGraph, RenderStats, SourceSnapshot, SourceTelemetry};

/// Keeps the capture pollers and the output stream alive.
///
/// Each capture stream is owned by its poller thread, so it stays open for
/// exactly as long as the poller runs; stopping joins the threads, which
/// drops the streams.
pub struct DeviceRun {
    _output: cpal::Stream,
    pollers: Vec<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    stats: RenderStats,
    source_telemetry: Vec<(BusId, SourceTelemetry)>,
    engine_latency_ms: f32,
}

impl DeviceRun {
    /// Live render statistics (updated by the output callback).
    pub fn stats(&self) -> RenderStatsSnapshot {
        self.stats.snapshot()
    }

    /// Per-source telemetry (peak/RMS/underflow/stall), callback-updated.
    pub fn source_snapshots(&self) -> Vec<SourceSnapshot> {
        RenderGraph::source_snapshots_from(&self.source_telemetry)
    }

    /// Engine-side latency for one block in milliseconds (device buffer
    /// excluded; add `describe_output`'s estimate for a round-trip figure).
    pub fn engine_latency_ms(&self) -> f32 {
        self.engine_latency_ms
    }
}

impl DeviceRun {
    /// Requests shutdown and waits for the capture pollers.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for poller in self.pollers.drain(..) {
            let _ = poller.join();
        }
    }
}

impl Drop for DeviceRun {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Opens the default (or selected) mic and system-loopback captures plus the
/// physical output, and starts rendering the mixer through it.
///
/// - `mic_device` / `system_device`: flexaudio device IDs (`None` = default).
/// - `output_device`: exact cpal device name (`None` = default output).
pub fn start(
    mic_device: Option<String>,
    system_device: Option<String>,
    output_device: Option<String>,
    engine: Engine,
    block_frames: usize,
) -> Result<DeviceRun, IoError> {
    let host = cpal::default_host();
    let device = match &output_device {
        Some(name) => host
            .output_devices()?
            .find(|d| d.name().map(|n| n == *name).unwrap_or(false))
            .ok_or_else(|| IoError::DeviceNotFound(name.clone()))?,
        None => host
            .default_output_device()
            .ok_or(IoError::NoOutputDevice)?,
    };
    let config = device.default_output_config()?;
    let device_rate = config.sample_rate().0;
    let device_channels = config.channels() as usize;
    let sample_format = config.sample_format();
    let stream_config = config.config();

    let mut graph = RenderGraph::new(engine, block_frames, device_rate, device_channels, 8192)
        .map_err(|err| IoError::UnsupportedFormat(err.to_string()))?;
    let engine_latency_ms = graph.engine_latency_ms();
    let (stats, source_telemetry) = graph.telemetry_handles();

    let stop = Arc::new(AtomicBool::new(false));
    let mut pollers = Vec::new();

    let open = |device_id: Option<String>, kind: flexaudio::SourceKind| {
        flexaudio::open(flexaudio::StreamConfig {
            device_id,
            kind,
            exclude_self: true, // never capture our own monitor output
            ..Default::default()
        })
    };

    // Default layout: bus 4 "Mic", bus 5 "System".
    if let Ok(stream) = open(mic_device, flexaudio::SourceKind::Mic) {
        pollers.push(attach_polled_source(&mut graph, stream, BusId(4), &stop)?);
    }
    if let Ok(stream) = open(system_device, flexaudio::SourceKind::SystemLoopback) {
        pollers.push(attach_polled_source(&mut graph, stream, BusId(5), &stop)?);
    }

    let output = build_output_stream(&device, &stream_config, sample_format, graph)?;
    output.play()?;

    Ok(DeviceRun {
        _output: output,
        pollers,
        stop,
        stats,
        source_telemetry,
        engine_latency_ms,
    })
}

fn attach_polled_source(
    graph: &mut RenderGraph,
    stream: flexaudio::Stream,
    bus: BusId,
    stop: &Arc<AtomicBool>,
) -> Result<JoinHandle<()>, IoError> {
    let mut producer = graph
        .attach_source(bus, 2, CANONICAL_SAMPLE_RATE, 32, 8192)
        .map_err(|err| IoError::UnsupportedFormat(err.to_string()))?;
    let stop = Arc::clone(stop);
    Ok(std::thread::spawn(move || {
        let mut stream = stream;
        while !stop.load(Ordering::Relaxed) {
            match stream.poll_capture() {
                Some(chunk) if chunk.frames > 0 => {
                    let channels = (chunk.data.len() / chunk.frames).max(1);
                    if producer
                        .push(CaptureChunk {
                            samples: chunk.data,
                            channels,
                            // flexaudio's tap output is canonical 48 kHz.
                            rate: CANONICAL_SAMPLE_RATE,
                        })
                        .is_err()
                    {
                        tracing::warn!(bus = bus.0, "capture queue full; dropping chunk");
                    }
                }
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }))
}

fn build_output_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: cpal::SampleFormat,
    graph: RenderGraph,
) -> Result<cpal::Stream, IoError> {
    let err_fn = |err| tracing::error!(%err, "output stream error");
    macro_rules! build {
        ($t:ty) => {{
            let mut graph = graph;
            device.build_output_stream(
                config,
                move |data: &mut [$t], _: &cpal::OutputCallbackInfo| graph.render(data),
                err_fn,
                None,
            )
        }};
    }
    match format {
        cpal::SampleFormat::I8 => build!(i8),
        cpal::SampleFormat::I16 => build!(i16),
        cpal::SampleFormat::I24 => build!(cpal::I24),
        cpal::SampleFormat::I32 => build!(i32),
        cpal::SampleFormat::I64 => build!(i64),
        cpal::SampleFormat::U8 => build!(u8),
        cpal::SampleFormat::U16 => build!(u16),
        cpal::SampleFormat::U32 => build!(u32),
        cpal::SampleFormat::U64 => build!(u64),
        cpal::SampleFormat::F32 => build!(f32),
        cpal::SampleFormat::F64 => build!(f64),
        other => return Err(IoError::UnsupportedFormat(format!("{other:?}"))),
    }
    .map_err(IoError::from)
}
