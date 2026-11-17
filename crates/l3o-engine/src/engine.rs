//! The mixer graph: buses feed two independent mixes (Monitor and Stream).
//!
//! The engine is owned by the render thread only. Control changes arrive
//! over a bounded queue and are drained once per block (see
//! [`crate::control`] and `docs/rt-design.md`). Every amplitude change runs
//! through the smoothed `Gain` node, so no control produces a click.

use l3o_core::{Bus, BusId, MAX_CHANNELS, Master, MixId, Preset, PresetError, Send};
use l3o_dsp::{AudioBlock, Gain, Processor};

use crate::control::{Control, ControlReceiver, ControlSender, control_queue};

/// Why an engine could not be constructed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EngineError {
    #[error("unsupported channel count {0}; supported: 1..={MAX_CHANNELS}")]
    UnsupportedChannels(usize),

    #[error("invalid preset: {0}")]
    InvalidPreset(#[from] PresetError),
}

/// Per-block accumulator for both mixes.
#[derive(Debug, Clone)]
pub struct MixAccumulator {
    pub monitor: Vec<f32>,
    pub stream: Vec<f32>,
}

impl MixAccumulator {
    pub fn new(frames: usize, channels: usize) -> Self {
        let len = frames * channels;
        Self {
            monitor: vec![0.0; len],
            stream: vec![0.0; len],
        }
    }

    pub fn clear(&mut self) {
        self.monitor.fill(0.0);
        self.stream.fill(0.0);
    }

    pub fn len(&self) -> usize {
        self.monitor.len()
    }

    pub fn is_empty(&self) -> bool {
        self.monitor.is_empty()
    }
}

struct BusRuntime {
    bus: Bus,
    gain: Gain,
    send_monitor: Gain,
    send_stream: Gain,
}

/// The real-time mixer.
pub struct Engine {
    sample_rate: u32,
    channels: usize,
    buses: Vec<BusRuntime>,
    master_monitor: Gain,
    master_stream: Gain,
    scratch: Vec<f32>,
    controls: ControlReceiver,
    sender: Option<ControlSender>,
    ignored_controls: u64,
}

impl Engine {
    pub fn new(preset: &Preset, sample_rate: u32, channels: usize) -> Result<Self, EngineError> {
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(EngineError::UnsupportedChannels(channels));
        }
        preset.validate()?;

        let buses = preset
            .buses
            .iter()
            .map(|bus| BusRuntime {
                bus: bus.clone(),
                gain: Gain::new(sample_rate, 10.0),
                send_monitor: Gain::new(sample_rate, 10.0),
                send_stream: Gain::new(sample_rate, 10.0),
            })
            .collect();

        let (sender, receiver) = control_queue();
        let mut engine = Self {
            sample_rate,
            channels,
            buses,
            master_monitor: Gain::new(sample_rate, 10.0),
            master_stream: Gain::new(sample_rate, 10.0),
            scratch: Vec::new(),
            controls: receiver,
            sender: Some(sender),
            ignored_controls: 0,
        };
        engine.apply_master(&preset.master);
        // Start at the configured gains instead of ramping from unity, so a
        // preset that begins muted is silent from the first sample.
        for runtime in &mut engine.buses {
            runtime.gain.set_target(runtime.bus.linear_gain());
            runtime
                .send_monitor
                .set_target(runtime.bus.sends.monitor.linear_gain());
            runtime
                .send_stream
                .set_target(runtime.bus.sends.stream.linear_gain());
            runtime.gain.reset();
            runtime.send_monitor.reset();
            runtime.send_stream.reset();
        }
        engine.master_monitor.reset();
        engine.master_stream.reset();
        Ok(engine)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Takes this engine's control sender. Consumes the only sender; a
    /// second call returns `None`.
    pub fn take_control_sender(&mut self) -> Option<ControlSender> {
        self.sender.take()
    }

    /// Number of control messages ignored because they targeted an unknown bus.
    pub fn ignored_controls(&self) -> u64 {
        self.ignored_controls
    }

    /// Drains pending controls. The render thread calls this exactly once
    /// per block, before processing any bus.
    pub fn drain_controls(&mut self) {
        while let Some(control) = self.controls.pop() {
            self.apply(control);
        }
    }

    fn apply(&mut self, control: Control) {
        match control {
            Control::SetBusGain { id, gain_db, muted } => {
                if let Some(runtime) = self.bus_mut(id) {
                    runtime.bus.gain_db = gain_db;
                    runtime.bus.muted = muted;
                    runtime.gain.set_target(runtime.bus.linear_gain());
                } else {
                    self.ignored_controls += 1;
                }
            }
            Control::SetBusSend { id, mix, send } => {
                if let Some(runtime) = self.bus_mut(id) {
                    *runtime.bus.sends.send_mut(mix) = send;
                    let target = send.linear_gain();
                    match mix {
                        MixId::Monitor => runtime.send_monitor.set_target(target),
                        MixId::Stream => runtime.send_stream.set_target(target),
                    }
                } else {
                    self.ignored_controls += 1;
                }
            }
            Control::SetMaster { master } => self.apply_master(&master),
        }
    }

    fn bus_mut(&mut self, id: BusId) -> Option<&mut BusRuntime> {
        self.buses.iter_mut().find(|b| b.bus.id == id)
    }

    /// Re-applies gains that may have changed on the UI thread.
    pub fn apply_preset(&mut self, preset: &Preset) {
        for runtime in &mut self.buses {
            if let Some(bus) = preset.bus(runtime.bus.id) {
                runtime.bus = bus.clone();
                runtime.gain.set_target(bus.linear_gain());
                runtime
                    .send_monitor
                    .set_target(bus.sends.monitor.linear_gain());
                runtime
                    .send_stream
                    .set_target(bus.sends.stream.linear_gain());
            }
        }
        self.apply_master(&preset.master);
    }

    fn apply_master(&mut self, master: &Master) {
        self.master_monitor
            .set_target(master.linear_gain(MixId::Monitor));
        self.master_stream
            .set_target(master.linear_gain(MixId::Stream));
    }

    pub fn prepare(&mut self, max_block_frames: usize) {
        for runtime in &mut self.buses {
            runtime.gain.prepare(self.sample_rate, max_block_frames);
            runtime
                .send_monitor
                .prepare(self.sample_rate, max_block_frames);
            runtime
                .send_stream
                .prepare(self.sample_rate, max_block_frames);
        }
        self.master_monitor
            .prepare(self.sample_rate, max_block_frames);
        self.master_stream
            .prepare(self.sample_rate, max_block_frames);
        self.scratch.clear();
        self.scratch.resize(max_block_frames * self.channels, 0.0);
    }

    /// Immediate (unsmoothed) send update kept for tests and tooling; the
    /// control queue is the production path.
    pub fn set_bus_send(&mut self, id: BusId, mix: MixId, send: Send) {
        self.apply(Control::SetBusSend { id, mix, send });
    }

    /// Processes one bus and adds it to both mix accumulators.
    ///
    /// `input` is modified in place (bus gain is applied before the sends).
    pub fn process(&mut self, id: BusId, input: &mut [f32], out: &mut MixAccumulator) {
        let Some(index) = self.buses.iter().position(|b| b.bus.id == id) else {
            return;
        };
        let channels = self.channels;
        let sample_rate = self.sample_rate;
        let runtime = &mut self.buses[index];

        let mut block = AudioBlock::new(input, channels, sample_rate);
        runtime.gain.process(&mut block);

        // Send gains run through the smoothed nodes too; a scratch copy keeps
        // the two mixes independent without allocating on the render path.
        self.scratch.copy_from_slice(input);
        let mut monitor_block = AudioBlock::new(&mut self.scratch, channels, sample_rate);
        runtime.send_monitor.process(&mut monitor_block);
        for (dst, src) in out.monitor.iter_mut().zip(self.scratch.iter()) {
            *dst += *src;
        }

        self.scratch.copy_from_slice(input);
        let mut stream_block = AudioBlock::new(&mut self.scratch, channels, sample_rate);
        runtime.send_stream.process(&mut stream_block);
        for (dst, src) in out.stream.iter_mut().zip(self.scratch.iter()) {
            *dst += *src;
        }
    }

    /// Applies the master gains. Call once per block, after all buses.
    pub fn finish(&mut self, out: &mut MixAccumulator) {
        let mut monitor = AudioBlock::new(&mut out.monitor, self.channels, self.sample_rate);
        self.master_monitor.process(&mut monitor);

        let mut stream = AudioBlock::new(&mut out.stream, self.channels, self.sample_rate);
        self.master_stream.process(&mut stream);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use l3o_core::{Preset, Send};

    const SR: u32 = 48_000;

    fn engine(preset: &Preset) -> Engine {
        Engine::new(preset, SR, 2).unwrap()
    }

    #[test]
    fn rejects_unsupported_channel_counts() {
        let preset = Preset::default();
        assert!(matches!(
            Engine::new(&preset, SR, 0),
            Err(EngineError::UnsupportedChannels(0))
        ));
        assert!(matches!(
            Engine::new(&preset, SR, 3),
            Err(EngineError::UnsupportedChannels(3))
        ));
    }

    #[test]
    fn rejects_invalid_preset() {
        let mut preset = Preset::default();
        preset.buses.push(l3o_core::Bus::new(0, "Dupe"));
        assert!(matches!(
            Engine::new(&preset, SR, 2),
            Err(EngineError::InvalidPreset(_))
        ));
    }

    #[test]
    fn unknown_bus_control_is_ignored_and_counted() {
        let preset = Preset::default();
        let mut engine = engine(&preset);
        let mut sender = engine.take_control_sender().unwrap();
        sender
            .send(Control::SetBusGain {
                id: BusId(99),
                gain_db: -6.0,
                muted: false,
            })
            .unwrap();
        drop(sender);
        engine.drain_controls();
        assert_eq!(engine.ignored_controls(), 1);
        assert!(engine.take_control_sender().is_none());
    }

    #[test]
    fn only_enabled_sends_receive_audio() {
        let mut preset = Preset::default();
        preset.bus_mut(BusId(0)).unwrap().sends.stream = Send::disabled();
        let mut engine = engine(&preset);
        engine.prepare(4);
        let mut out = MixAccumulator::new(4, 2);
        let mut input = vec![0.5f32; 8];

        engine.process(BusId(0), &mut input, &mut out);

        assert!(out.monitor.iter().any(|s| *s != 0.0));
        assert!(out.stream.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn muted_bus_is_silent() {
        let mut preset = Preset::default();
        preset.bus_mut(BusId(0)).unwrap().muted = true;
        let mut engine = engine(&preset);
        engine.prepare(4);
        let mut out = MixAccumulator::new(4, 2);
        let mut input = vec![0.5f32; 8];

        engine.process(BusId(0), &mut input, &mut out);
        assert!(out.monitor.iter().all(|s| s.abs() < 1e-3));
        assert!(out.stream.iter().all(|s| s.abs() < 1e-3));
    }

    #[test]
    fn master_mute_only_affects_one_mix() {
        let mut preset = Preset::default();
        preset.master.stream_muted = true;
        let mut engine = engine(&preset);
        engine.prepare(4);

        let mut out = MixAccumulator::new(4, 2);
        let mut input = vec![0.25f32; 8];
        engine.process(BusId(0), &mut input, &mut out);
        engine.finish(&mut out);

        assert!(out.monitor.iter().any(|s| *s != 0.0));
        assert!(out.stream.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn buses_accumulate() {
        let preset = Preset::default();
        let mut engine = engine(&preset);
        engine.prepare(2);
        let mut out = MixAccumulator::new(2, 2);

        let mut a = vec![0.1f32; 4];
        let mut b = vec![0.2f32; 4];
        engine.process(BusId(0), &mut a, &mut out);
        engine.process(BusId(1), &mut b, &mut out);

        assert!(out.monitor.iter().all(|s| (*s - 0.3).abs() < 1e-6));
    }

    #[test]
    fn unknown_bus_is_ignored() {
        let preset = Preset::default();
        let mut engine = engine(&preset);
        let mut out = MixAccumulator::new(2, 2);
        let mut input = vec![1.0f32; 4];
        engine.process(BusId(99), &mut input, &mut out);
        assert!(out.monitor.iter().all(|s| *s == 0.0));
    }
}
