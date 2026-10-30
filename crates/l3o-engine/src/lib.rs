//! The mixer graph: buses feed two independent mixes (Monitor and Stream).
//!
//! This is the M0 skeleton. It already implements the Streamer Mode routing
//! matrix so the behaviour can be tested without any audio device.

use l3o_core::{Bus, BusId, Preset};
use l3o_dsp::{AudioBlock, Gain, Processor};

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
}

/// The real-time mixer.
pub struct Engine {
    sample_rate: u32,
    channels: usize,
    buses: Vec<BusRuntime>,
    master_monitor: Gain,
    master_stream: Gain,
}

impl Engine {
    pub fn new(preset: &Preset, sample_rate: u32, channels: usize) -> Self {
        assert!(channels > 0, "channels must be greater than zero");

        let buses = preset
            .buses
            .iter()
            .map(|bus| BusRuntime {
                bus: bus.clone(),
                gain: Gain::new(sample_rate, 10.0),
            })
            .collect();

        let mut engine = Self {
            sample_rate,
            channels,
            buses,
            master_monitor: Gain::new(sample_rate, 10.0),
            master_stream: Gain::new(sample_rate, 10.0),
        };
        engine.apply_master(&preset.master);
        // Start at the configured gains instead of ramping from unity, so a
        // preset that begins muted is silent from the first sample.
        for runtime in &mut engine.buses {
            runtime.gain.set_target(runtime.bus.linear_gain());
            runtime.gain.reset();
        }
        engine.master_monitor.reset();
        engine.master_stream.reset();
        engine
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Re-applies gains that may have changed on the UI thread.
    pub fn apply_preset(&mut self, preset: &Preset) {
        for runtime in &mut self.buses {
            if let Some(bus) = preset.bus(runtime.bus.id) {
                runtime.bus = bus.clone();
                runtime.gain.set_target(bus.linear_gain());
            }
        }
        self.apply_master(&preset.master);
    }

    fn apply_master(&mut self, master: &l3o_core::Master) {
        self.master_monitor
            .set_target(master.linear_gain(l3o_core::MixId::Monitor));
        self.master_stream
            .set_target(master.linear_gain(l3o_core::MixId::Stream));
    }

    pub fn prepare(&mut self, max_block_frames: usize) {
        for runtime in &mut self.buses {
            runtime.gain.prepare(self.sample_rate, max_block_frames);
        }
        self.master_monitor
            .prepare(self.sample_rate, max_block_frames);
        self.master_stream
            .prepare(self.sample_rate, max_block_frames);
    }

    pub fn set_bus_send(&mut self, id: BusId, mix: l3o_core::MixId, send: l3o_core::Send) {
        if let Some(runtime) = self.buses.iter_mut().find(|b| b.bus.id == id) {
            *runtime.bus.sends.send_mut(mix) = send;
        }
    }

    /// Processes one bus and adds it to both mix accumulators.
    ///
    /// `input` is modified in place (bus gain is applied before the sends).
    pub fn process(&mut self, id: BusId, input: &mut [f32], out: &mut MixAccumulator) {
        let Some(runtime) = self.buses.iter_mut().find(|b| b.bus.id == id) else {
            return;
        };

        let mut block = AudioBlock::new(input, self.channels, self.sample_rate);
        runtime.gain.process(&mut block);

        let monitor_gain = runtime.bus.sends.monitor.linear_gain();
        let stream_gain = runtime.bus.sends.stream.linear_gain();

        if monitor_gain != 0.0 {
            for (dst, src) in out.monitor.iter_mut().zip(input.iter()) {
                *dst += src * monitor_gain;
            }
        }
        if stream_gain != 0.0 {
            for (dst, src) in out.stream.iter_mut().zip(input.iter()) {
                *dst += src * stream_gain;
            }
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
    use l3o_core::{MixId, Preset, Send};

    const SR: u32 = 48_000;

    #[test]
    fn only_enabled_sends_receive_audio() {
        let preset = Preset::default();
        let mut engine = Engine::new(&preset, SR, 2);
        let mut out = MixAccumulator::new(4, 2);
        let mut input = vec![0.5f32; 8];

        engine.set_bus_send(BusId(0), MixId::Stream, Send::disabled());
        engine.process(BusId(0), &mut input, &mut out);

        assert!(out.monitor.iter().any(|s| *s != 0.0));
        assert!(out.stream.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn muted_bus_is_silent() {
        let mut preset = Preset::default();
        preset.bus_mut(BusId(0)).unwrap().muted = true;
        let mut engine = Engine::new(&preset, SR, 2);
        let mut out = MixAccumulator::new(4, 2);
        let mut input = vec![0.5f32; 8];

        engine.process(BusId(0), &mut input, &mut out);
        // The gain ramps down, so check the last samples.
        assert!(out.monitor.iter().all(|s| s.abs() < 1e-3));
        assert!(out.stream.iter().all(|s| s.abs() < 1e-3));
    }

    #[test]
    fn master_mute_only_affects_one_mix() {
        let mut preset = Preset::default();
        preset.master.stream_muted = true;
        let mut engine = Engine::new(&preset, SR, 2);
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
        let mut engine = Engine::new(&preset, SR, 2);
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
        let mut engine = Engine::new(&preset, SR, 2);
        let mut out = MixAccumulator::new(2, 2);
        let mut input = vec![1.0f32; 4];
        engine.process(BusId(99), &mut input, &mut out);
        assert!(out.monitor.iter().all(|s| *s == 0.0));
    }
}
