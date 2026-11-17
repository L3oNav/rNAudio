//! The render graph: engine + capture sources + device output.
//!
//! [`RenderGraph::render`] is the cpal output callback body: it drains the
//! control queue, pulls every capture source forward by one engine block,
//! runs the mixer, converts to the device rate/channels/format, and copies
//! into the callback buffer. Everything is preallocated; bounded queues
//! report overflow/underflow through [`RenderStats`] instead of blocking.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use l3o_core::{BusId, CANONICAL_SAMPLE_RATE, MAX_CHANNELS};
use l3o_engine::{Engine, MixAccumulator};

use crate::convert::{ConvertError, LinearResampler, adapt_channels, to_device_samples};

/// One chunk of captured audio, as delivered by the capture backend.
#[derive(Debug)]
pub struct CaptureChunk {
    /// Interleaved `f32` samples.
    pub samples: Vec<f32>,
    pub channels: usize,
    pub rate: u32,
}

/// Bounded queue between a capture thread and the render callback.
pub fn chunk_queue(
    capacity: usize,
) -> (rtrb::Producer<CaptureChunk>, rtrb::Consumer<CaptureChunk>) {
    rtrb::RingBuffer::new(capacity)
}

/// Per-source telemetry, readable from any thread. Written once per block
/// by the render callback (atomic stores only), reset by
/// [`TelemetryHandle::reset`].
#[derive(Clone, Default)]
pub struct SourceTelemetry {
    /// Peak magnitude of the last block (`f32` bits).
    pub peak: Arc<AtomicU64>,
    /// RMS level of the last block (`f32` bits).
    pub rms: Arc<AtomicU64>,
    /// Blocks that ran short (source starved).
    pub underflows: Arc<AtomicU64>,
    /// Whether the last block ran short.
    pub stalled: Arc<AtomicUsize>,
    /// Chunks waiting in the queue when last observed.
    pub queue_fill: Arc<AtomicUsize>,
}

impl SourceTelemetry {
    pub fn record_block(&self, block: &[f32], short: bool) {
        let peak = block.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        let rms = if block.is_empty() {
            0.0
        } else {
            (block.iter().map(|s| s * s).sum::<f32>() / block.len() as f32).sqrt()
        };
        self.peak.store(peak.to_bits() as u64, Ordering::Relaxed);
        self.rms.store(rms.to_bits() as u64, Ordering::Relaxed);
        if short {
            self.underflows.fetch_add(1, Ordering::Relaxed);
            self.stalled.store(1, Ordering::Relaxed);
        } else {
            self.stalled.store(0, Ordering::Relaxed);
        }
    }

    pub fn reset(&self) {
        self.peak.store(0, Ordering::Relaxed);
        self.rms.store(0, Ordering::Relaxed);
        self.underflows.store(0, Ordering::Relaxed);
        self.stalled.store(0, Ordering::Relaxed);
        self.queue_fill.store(0, Ordering::Relaxed);
    }
}

/// Counters describing render health, readable from any thread.
#[derive(Clone, Default)]
pub struct RenderStats {
    /// Capture chunks dropped because the queue was full (producer side
    /// detects the push failure; the graph counts what it sees).
    pub chunk_overflows: Arc<AtomicU64>,
    /// Engine blocks that ran with at least one source short of audio.
    pub source_underflows: Arc<AtomicU64>,
    /// Chunks a source resampler rejected (staging full).
    pub conversion_overflows: Arc<AtomicU64>,
    /// Cumulative callback processing time, for the average.
    pub callback_nanos_total: Arc<AtomicU64>,
    /// Peak callback processing time seen so far.
    pub callback_nanos_max: Arc<AtomicU64>,
    /// Blocks rendered so far.
    pub blocks_rendered: Arc<AtomicU64>,
}

impl RenderStats {
    pub fn snapshot(&self) -> RenderStatsSnapshot {
        let total = self.callback_nanos_total.load(Ordering::Relaxed);
        let blocks = self.blocks_rendered.load(Ordering::Relaxed);
        RenderStatsSnapshot {
            chunk_overflows: self.chunk_overflows.load(Ordering::Relaxed),
            source_underflows: self.source_underflows.load(Ordering::Relaxed),
            conversion_overflows: self.conversion_overflows.load(Ordering::Relaxed),
            callback_nanos_max: self.callback_nanos_max.load(Ordering::Relaxed),
            callback_nanos_avg: total.checked_div(blocks).unwrap_or(0),
            blocks_rendered: blocks,
        }
    }
}

/// Plain per-source view for IPC and tests.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceSnapshot {
    pub bus: u32,
    pub peak: f32,
    pub rms: f32,
    pub underflows: u64,
    pub stalled: bool,
    pub queue_fill: usize,
    /// Frames buffered in the source resampler.
    pub resampler_fill: usize,
}

/// Plain view of [`RenderStats`], safe to hand across IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct RenderStatsSnapshot {
    pub chunk_overflows: u64,
    pub source_underflows: u64,
    pub conversion_overflows: u64,
    pub callback_nanos_max: u64,
    pub callback_nanos_avg: u64,
    pub blocks_rendered: u64,
}

/// How many frames one engine block yields at the device rate.
fn device_frames_per_block(block_frames: usize, device_rate: u32) -> usize {
    (block_frames as u64 * device_rate as u64).div_ceil(CANONICAL_SAMPLE_RATE as u64) as usize
        + MAX_CHANNELS // interpolation slack
}

struct SourceSlot {
    bus: BusId,
    queue: rtrb::Consumer<CaptureChunk>,
    resampler: LinearResampler,
    /// Stereo adaptation scratch for one chunk.
    chunk_scratch: Vec<f32>,
    /// Assembled canonical block for this source; `filled` frames are valid.
    block: Vec<f32>,
    filled: usize,
    telemetry: SourceTelemetry,
}

impl SourceSlot {
    fn pull_chunks(&mut self, stats: &RenderStats) {
        while let Ok(chunk) = self.queue.pop() {
            if chunk.channels == 0 || chunk.rate == 0 {
                continue;
            }
            let frames = chunk.samples.len() / chunk.channels;
            if frames == 0 {
                continue;
            }
            let need = frames * MAX_CHANNELS;
            if self.chunk_scratch.len() < need {
                // Allocation on the render path; see the shortcut note on
                // RenderGraph.
                self.chunk_scratch.resize(need, 0.0);
            }
            if let Err(err) =
                adapt_channels(&chunk.samples, chunk.channels, &mut self.chunk_scratch)
            {
                tracing::warn!(bus = self.bus.0, %err, "dropping malformed capture chunk");
                continue;
            }
            if let Err(err) = self.resampler.push(&self.chunk_scratch[..need]) {
                stats.conversion_overflows.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(bus = self.bus.0, %err, "capture resampler staging full");
            }
        }
    }

    /// Fills `block` with exactly `block_frames` canonical frames. Returns
    /// false when the source ran dry (block zero-padded); the source keeps
    /// working and resumes on its own — only this source is degraded.
    fn fill_block(&mut self, block_frames: usize) -> bool {
        while self.filled < block_frames && self.resampler.available() > 0 {
            let take = (block_frames - self.filled).min(self.resampler.available());
            let start = self.filled * MAX_CHANNELS;
            let src = self.resampler.output();
            self.block[start..start + take * MAX_CHANNELS]
                .copy_from_slice(&src[..take * MAX_CHANNELS]);
            self.resampler.consume(take);
            self.filled += take;
        }
        let full = self.filled == block_frames;
        if !full {
            self.block[self.filled * MAX_CHANNELS..].fill(0.0);
        }
        self.telemetry.record_block(&self.block, !full);
        self.telemetry
            .queue_fill
            .store(self.queue.slots(), Ordering::Relaxed);
        self.filled = 0;
        full
    }
}

/// Engine plus I/O plumbing, designed to live inside the cpal output callback.
///
/// shortcut: per-chunk channel adaptation grows `chunk_scratch` on first
/// sight of a larger chunk, which allocates on the render path. flexaudio
/// chunks are a few hundred frames, so this happens at most once per source
/// after startup; remove by preallocating for the device-reported channel
/// count in P1.4 when device selection lands.
pub struct RenderGraph {
    engine: Engine,
    block_frames: usize,
    device_channels: usize,
    /// 48 kHz stereo -> device rate stereo; None when the device is already
    /// canonical stereo.
    out_resampler: Option<LinearResampler>,
    sources: Vec<SourceSlot>,
    accum: MixAccumulator,
    /// Engine monitor block adapted to device channels.
    device_block: Vec<f32>,
    /// Resampled output waiting for the next callback.
    device_pending: Vec<f32>,
    device_pending_len: usize,
    /// f32 staging for the current callback buffer.
    callback_staging: Vec<f32>,
    stats: RenderStats,
}

impl RenderGraph {
    /// `staging_frames` bounds per-source capture staging and the callback
    /// staging; larger chunks are rejected (and counted), never allocate.
    pub fn new(
        mut engine: Engine,
        block_frames: usize,
        device_rate: u32,
        device_channels: usize,
        staging_frames: usize,
    ) -> Result<Self, ConvertError> {
        if device_channels == 0 || device_channels > MAX_CHANNELS {
            return Err(ConvertError::UnsupportedChannels(device_channels));
        }
        engine.prepare(block_frames);
        let out_resampler = if device_rate == CANONICAL_SAMPLE_RATE {
            None
        } else {
            Some(LinearResampler::new(
                CANONICAL_SAMPLE_RATE,
                device_rate,
                MAX_CHANNELS,
                staging_frames,
            )?)
        };
        let pending_cap = device_frames_per_block(block_frames, device_rate) * device_channels;
        Ok(Self {
            engine,
            block_frames,
            device_channels,
            out_resampler,
            sources: Vec::new(),
            accum: MixAccumulator::new(block_frames, MAX_CHANNELS),
            device_block: vec![0.0; block_frames * device_channels],
            device_pending: vec![0.0; pending_cap],
            device_pending_len: 0,
            callback_staging: vec![0.0; staging_frames * MAX_CHANNELS],
            stats: RenderStats::default(),
        })
    }

    pub fn block_frames(&self) -> usize {
        self.block_frames
    }

    pub fn stats(&self) -> RenderStats {
        self.stats.clone()
    }

    /// Clones of every telemetry handle, for owners that outlive the move
    /// into the output callback (the daemon reads them for `Status`).
    pub fn telemetry_handles(&self) -> (RenderStats, Vec<(BusId, SourceTelemetry)>) {
        (
            self.stats.clone(),
            self.sources
                .iter()
                .map(|s| (s.bus, s.telemetry.clone()))
                .collect(),
        )
    }

    /// Builds source snapshots from telemetry handles (used by the daemon,
    /// which cannot reach the graph itself).
    pub fn source_snapshots_from(handles: &[(BusId, SourceTelemetry)]) -> Vec<SourceSnapshot> {
        handles
            .iter()
            .map(|(bus, t)| SourceSnapshot {
                bus: bus.0,
                peak: f32::from_bits(t.peak.load(Ordering::Relaxed) as u32),
                rms: f32::from_bits(t.rms.load(Ordering::Relaxed) as u32),
                underflows: t.underflows.load(Ordering::Relaxed),
                stalled: t.stalled.load(Ordering::Relaxed) != 0,
                queue_fill: t.queue_fill.load(Ordering::Relaxed),
                resampler_fill: 0, // only known inside the callback
            })
            .collect()
    }

    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    /// Attaches a capture source to a bus. Returns the producer end the
    /// capture thread pushes [`CaptureChunk`]s into. Per-chunk channels and
    /// rate come from each [`CaptureChunk`]; `source_rate` seeds the
    /// resampler and `_source_channels` is reserved for future validation.
    pub fn attach_source(
        &mut self,
        bus: BusId,
        _source_channels: usize,
        source_rate: u32,
        queue_capacity: usize,
        staging_frames: usize,
    ) -> Result<rtrb::Producer<CaptureChunk>, ConvertError> {
        let (producer, consumer) = chunk_queue(queue_capacity);
        self.sources.push(SourceSlot {
            bus,
            queue: consumer,
            resampler: LinearResampler::new(
                source_rate,
                CANONICAL_SAMPLE_RATE,
                MAX_CHANNELS,
                staging_frames,
            )?,
            chunk_scratch: vec![0.0; staging_frames * MAX_CHANNELS],
            block: vec![0.0; self.block_frames * MAX_CHANNELS],
            filled: 0,
            telemetry: SourceTelemetry::default(),
        });
        Ok(producer)
    }

    /// Runs the mixer for one engine block and converts the monitor mix to
    /// the device layout, appending to the pending output.
    fn produce_device_block(&mut self) {
        self.engine.drain_controls();

        let mut all_full = true;
        for source in &mut self.sources {
            source.pull_chunks(&self.stats);
            if !source.fill_block(self.block_frames) {
                all_full = false;
            }
        }
        if !all_full {
            self.stats.source_underflows.fetch_add(1, Ordering::Relaxed);
        }
        self.stats.blocks_rendered.fetch_add(1, Ordering::Relaxed);

        self.accum.clear();
        // Borrow split: engine and sources are disjoint fields.
        let engine = &mut self.engine;
        for source in &mut self.sources {
            let mut block = std::mem::take(&mut source.block);
            engine.process(source.bus, &mut block, &mut self.accum);
            source.block = block;
        }
        engine.finish(&mut self.accum);

        adapt_channels(&self.accum.monitor, MAX_CHANNELS, &mut self.device_block)
            .expect("engine output is stereo-adaptable");

        match &mut self.out_resampler {
            None => {
                let frames = self.block_frames * self.device_channels;
                self.device_pending[..frames].copy_from_slice(&self.device_block[..frames]);
                self.device_pending_len = frames;
            }
            Some(resampler) => {
                if let Err(err) = resampler.push(&self.device_block) {
                    self.stats
                        .conversion_overflows
                        .fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(%err, "output resampler staging full");
                }
                let take = resampler.available().min(
                    self.device_pending.len() / self.device_channels
                        - self.device_pending_len / self.device_channels,
                );
                let start = self.device_pending_len;
                let src = resampler.output();
                self.device_pending[start..start + take * self.device_channels]
                    .copy_from_slice(&src[..take * self.device_channels]);
                resampler.consume(take);
                self.device_pending_len += take * self.device_channels;
            }
        }
    }

    /// The cpal output callback body. Fills `dst` completely; if the engine
    /// has nothing the callback gets silence and the underflow counter moves.
    pub fn render<T>(&mut self, dst: &mut [T])
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        let started = std::time::Instant::now();
        self.render_inner(dst);
        let nanos = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.stats
            .callback_nanos_total
            .fetch_add(nanos, Ordering::Relaxed);
        self.stats
            .callback_nanos_max
            .fetch_max(nanos, Ordering::Relaxed);
    }

    /// Rough latency through engine + output resampler for one block, in
    /// milliseconds, excluding the device buffer. Add the device buffer
    /// estimate (`l3o_io::describe_output`) for a round-trip figure.
    pub fn engine_latency_ms(&self) -> f32 {
        let block_ms = self.block_frames as f32 / CANONICAL_SAMPLE_RATE as f32 * 1000.0;
        // The resampler holds back at most one frame for interpolation.
        block_ms + 1000.0 / CANONICAL_SAMPLE_RATE as f32
    }

    /// Per-source snapshots for IPC/telemetry consumers.
    pub fn source_snapshots(&self) -> Vec<SourceSnapshot> {
        self.sources
            .iter()
            .map(|s| SourceSnapshot {
                bus: s.bus.0,
                peak: f32::from_bits(s.telemetry.peak.load(Ordering::Relaxed) as u32),
                rms: f32::from_bits(s.telemetry.rms.load(Ordering::Relaxed) as u32),
                underflows: s.telemetry.underflows.load(Ordering::Relaxed),
                stalled: s.telemetry.stalled.load(Ordering::Relaxed) != 0,
                queue_fill: s.telemetry.queue_fill.load(Ordering::Relaxed),
                resampler_fill: s.resampler.available(),
            })
            .collect()
    }

    fn render_inner<T>(&mut self, dst: &mut [T])
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        let channels = self.device_channels;
        let mut written = 0;
        while written < dst.len() {
            if self.device_pending_len == 0 {
                self.produce_device_block();
            }
            if self.device_pending_len == 0 {
                // No device frames (e.g. resampler dry at startup): emit
                // silence for the rest of the callback rather than spin.
                for sample in dst[written..].iter_mut() {
                    *sample = T::from_sample(0.0);
                }
                return;
            }
            let want = ((dst.len() - written) / channels).min(self.device_pending_len / channels);
            let staging_need = want * channels;
            if staging_need > self.callback_staging.len() {
                // Defensive: absurd callback sizes are served in slices.
                let want = self.callback_staging.len() / channels;
                let staging_need = want * channels;
                self.callback_staging[..staging_need]
                    .copy_from_slice(&self.device_pending[..staging_need]);
                to_device_samples(
                    &self.callback_staging[..staging_need],
                    &mut dst[written..written + staging_need],
                );
                self.device_pending
                    .copy_within(staging_need..self.device_pending_len, 0);
                self.device_pending_len -= staging_need;
                written += staging_need;
                continue;
            }
            self.callback_staging[..staging_need]
                .copy_from_slice(&self.device_pending[..staging_need]);
            to_device_samples(
                &self.callback_staging[..staging_need],
                &mut dst[written..written + staging_need],
            );
            self.device_pending
                .copy_within(staging_need..self.device_pending_len, 0);
            self.device_pending_len -= staging_need;
            written += staging_need;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use l3o_core::Preset;

    const SR: u32 = 48_000;

    fn graph(block: usize, ch: usize) -> RenderGraph {
        let preset = Preset::default();
        let engine = Engine::new(&preset, SR, 2).unwrap();
        RenderGraph::new(engine, block, SR, ch, 4096).unwrap()
    }

    fn dc_chunk(value: f32, frames: usize, ch: usize) -> CaptureChunk {
        CaptureChunk {
            samples: vec![value; frames * ch],
            channels: ch,
            rate: SR,
        }
    }

    #[test]
    fn routes_capture_to_monitor_at_128_and_256() {
        for block in [128, 256] {
            let mut g = graph(block, 2);
            let mut producer = g.attach_source(BusId(0), 2, SR, 8, 4096).unwrap();
            producer.push(dc_chunk(0.5, block, 2)).unwrap();
            let mut out = vec![0.0f32; block * 2];
            g.render(&mut out);
            for (i, s) in out.iter().enumerate() {
                assert!(
                    (*s - 0.5).abs() < 1e-5,
                    "block {block}: sample {i} got {s}, want 0.5"
                );
            }
        }
    }

    #[test]
    fn bus_gain_applies_in_the_device_path() {
        let mut g = graph(128, 2);
        let mut sender = g.engine_mut().take_control_sender().unwrap();
        sender
            .send(l3o_engine::Control::SetBusGain {
                id: BusId(0),
                gain_db: -6.0,
                muted: false,
            })
            .unwrap();
        let mut producer = g.attach_source(BusId(0), 2, SR, 16, 4096).unwrap();
        for _ in 0..6 {
            producer.push(dc_chunk(0.5, 128, 2)).unwrap();
        }
        let expected = 0.5 * l3o_core::db_to_linear(-6.0);
        // Block 1 is mid-ramp; the last block must be fully converged.
        let mut out = vec![0.0f32; 128 * 2];
        for i in 0..6 {
            g.render(&mut out);
            if i < 4 {
                continue;
            }
            for (n, s) in out.iter().enumerate() {
                assert!(
                    (*s - expected).abs() < 1e-3,
                    "block {i} sample {n}: got {s}, want ~{expected}"
                );
            }
        }
    }

    #[test]
    fn sources_do_not_cross_buses() {
        let mut g = graph(128, 2);
        let mut p0 = g.attach_source(BusId(0), 2, SR, 8, 4096).unwrap();
        let mut p1 = g.attach_source(BusId(1), 2, SR, 8, 4096).unwrap();
        p0.push(dc_chunk(0.3, 128, 2)).unwrap();
        p1.push(dc_chunk(0.9, 128, 2)).unwrap();
        let mut out = vec![0.0f32; 128 * 2];
        g.render(&mut out);
        // Bus 0 has monitor unity, bus 1 monitor unity: both sum.
        assert!((out[0] - 1.2).abs() < 1e-4);
    }

    #[test]
    fn mono_capture_is_duplicated_to_stereo() {
        let mut g = graph(128, 2);
        let mut producer = g.attach_source(BusId(0), 1, SR, 8, 4096).unwrap();
        producer.push(dc_chunk(0.4, 128, 1)).unwrap();
        let mut out = vec![0.0f32; 128 * 2];
        g.render(&mut out);
        assert!((out[0] - 0.4).abs() < 1e-5);
        assert!((out[1] - 0.4).abs() < 1e-5);
    }

    #[test]
    fn non_canonical_source_rate_is_converted() {
        let mut g = graph(128, 2);
        let mut producer = g.attach_source(BusId(0), 2, 44_100, 8, 4096).unwrap();
        producer.push(dc_chunk(0.25, 441, 2)).unwrap();
        let mut out = vec![0.0f32; 128 * 2];
        g.render(&mut out);
        assert!(
            out.iter().all(|s| (*s - 0.25).abs() < 1e-2),
            "44.1k DC should pass through as ~0.25, got {:?}",
            &out[..4]
        );
    }

    #[test]
    fn dry_source_renders_silence_and_counts_underflow() {
        let mut g = graph(128, 2);
        g.attach_source(BusId(0), 2, SR, 8, 4096).unwrap();
        let mut out = vec![1.0f32; 128 * 2];
        g.render(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
        let stats = g.stats().snapshot();
        assert_eq!(stats.source_underflows, 1);
    }

    #[test]
    fn full_queue_reports_overflow_on_push() {
        let mut g = graph(128, 2);
        let mut producer = g.attach_source(BusId(0), 2, SR, 2, 4096).unwrap();
        producer.push(dc_chunk(0.1, 128, 2)).unwrap();
        producer.push(dc_chunk(0.1, 128, 2)).unwrap();
        // Third push exceeds capacity; the capture side must drop and count.
        assert!(producer.push(dc_chunk(0.1, 128, 2)).is_err());
        let mut out = vec![0.0f32; 128 * 2];
        g.render(&mut out);
        assert!(out.iter().all(|s| (*s - 0.1).abs() < 1e-5));
    }

    #[test]
    fn starved_source_recovers_and_never_affects_others() {
        let mut g = graph(128, 2);
        let mut p0 = g.attach_source(BusId(0), 2, SR, 8, 4096).unwrap();
        let mut p1 = g.attach_source(BusId(1), 2, SR, 8, 4096).unwrap();

        let mut out = vec![0.0f32; 128 * 2];

        // Steady state on both sources (one block each, fully consumed).
        p0.push(dc_chunk(0.3, 128, 2)).unwrap();
        p1.push(dc_chunk(0.5, 128, 2)).unwrap();
        g.render(&mut out);
        assert!((out[0] - 0.8).abs() < 1e-4);

        // Source 0 stalls for several blocks (stream error, device glitch).
        for _ in 0..3 {
            p1.push(dc_chunk(0.5, 128, 2)).unwrap();
            g.render(&mut out);
            // Only bus 1 contributes; bus 0 was zero-padded, not skipped.
            assert!(
                (out[0] - 0.5).abs() < 1e-4,
                "starved source leaked or ate the other bus"
            );
        }
        let starved = &g.source_snapshots()[0];
        assert!(starved.stalled);
        assert!(starved.underflows >= 1);

        // Source 0 recovers on its own; the graph never restarted anything.
        p0.push(dc_chunk(0.3, 128, 2)).unwrap();
        p1.push(dc_chunk(0.5, 128, 2)).unwrap();
        g.render(&mut out);
        assert!((out[0] - 0.8).abs() < 1e-4);
        assert!(!g.source_snapshots()[0].stalled);
    }

    #[test]
    fn frame_gaps_are_zero_padded_not_propagated() {
        let mut g = graph(128, 2);
        let mut producer = g.attach_source(BusId(0), 2, SR, 8, 4096).unwrap();
        producer.push(dc_chunk(0.7, 40, 2)).unwrap(); // partial
        let mut out = vec![0.0f32; 128 * 2];
        g.render(&mut out);
        // First 40 frames carry audio, the rest is padded silence.
        assert!((out[0] - 0.7).abs() < 1e-5);
        assert_eq!(out[127 * 2], 0.0);
        assert_eq!(g.stats().snapshot().source_underflows, 1);

        // Next block continues seamlessly once data returns.
        producer.push(dc_chunk(0.7, 128, 2)).unwrap();
        g.render(&mut out);
        assert!((out[0] - 0.7).abs() < 1e-5);
    }

    #[test]
    fn independent_clock_drift_stays_bounded() {
        // Source B claims 48 kHz but delivers at 48.048 kHz (+0.1% clock):
        // 48 real frames every block disguised as canonical chunks. The
        // resampler must absorb the drift without unbounded growth.
        let mut g = graph(128, 2);
        let mut slow = g.attach_source(BusId(0), 2, SR, 8, 4096).unwrap();
        let mut fast = g.attach_source(BusId(1), 2, SR, 8, 4096).unwrap();
        let mut out = vec![0.0f32; 128 * 2];
        for block in 0..600 {
            // 600 blocks x 128 frames = 10 s of audio.
            slow.push(dc_chunk(0.1, 128, 2)).unwrap();
            let skewed = 128 + if block % 10 == 0 { 1 } else { 0 }; // +0.1%
            fast.push(dc_chunk(0.2, skewed, 2)).unwrap();
            g.render(&mut out);
        }
        let snapshot = g.stats().snapshot();
        assert_eq!(
            snapshot.conversion_overflows, 0,
            "drift exhausted the resampler staging: {snapshot:?}"
        );
        let fast_snap = &g.source_snapshots()[1];
        assert!(
            fast_snap.resampler_fill < 4096,
            "resampler grew without bound: {}",
            fast_snap.resampler_fill
        );
        // Both sources still contribute audio after 10 s of drift.
        assert!((out[0] - 0.3).abs() < 1e-2);
    }

    #[test]
    fn telemetry_reports_peak_rms_and_callback_time() {
        let mut g = graph(128, 2);
        let mut producer = g.attach_source(BusId(0), 2, SR, 8, 4096).unwrap();
        producer.push(dc_chunk(0.5, 128, 2)).unwrap();
        let mut out = vec![0.0f32; 128 * 2];
        g.render(&mut out);
        let snap = &g.source_snapshots()[0];
        assert!((snap.peak - 0.5).abs() < 1e-5);
        assert!((snap.rms - 0.5).abs() < 1e-5);
        let stats = g.stats().snapshot();
        assert_eq!(stats.blocks_rendered, 1);
        assert!(stats.callback_nanos_max > 0);
        assert!(stats.callback_nanos_avg > 0);
    }

    #[test]
    fn engine_latency_is_block_size_bounded() {
        let g128 = graph(128, 2);
        let g256 = graph(256, 2);
        assert!(g128.engine_latency_ms() < g256.engine_latency_ms());
        assert!(
            g256.engine_latency_ms() < 10.0,
            "engine latency must stay sub-block"
        );
    }

    #[test]
    fn partial_chunks_accumulate_into_a_block() {
        let mut g = graph(128, 2);
        let mut producer = g.attach_source(BusId(0), 2, SR, 8, 4096).unwrap();
        for _ in 0..4 {
            producer.push(dc_chunk(0.2, 37, 2)).unwrap();
        }
        // 148 frames pushed; one 128-frame block renders, 20 carry over.
        let mut out = vec![0.0f32; 128 * 2];
        g.render(&mut out);
        assert!(out.iter().all(|s| (*s - 0.2).abs() < 1e-5));
    }
}
