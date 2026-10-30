//! Real-time DSP primitives and effect nodes.
//!
//! Every [`Processor`] is designed to run inside the audio callback: no
//! allocation, no locking and no panics on the hot path.

/// An interleaved block of `f32` samples.
#[derive(Debug)]
pub struct AudioBlock<'a> {
    pub data: &'a mut [f32],
    pub channels: usize,
    pub sample_rate: u32,
}

impl<'a> AudioBlock<'a> {
    pub fn new(data: &'a mut [f32], channels: usize, sample_rate: u32) -> Self {
        debug_assert!(channels > 0);
        debug_assert_eq!(data.len() % channels, 0);
        Self {
            data,
            channels,
            sample_rate,
        }
    }

    pub fn frames(&self) -> usize {
        self.data.len() / self.channels
    }
}

/// A single node in a processing chain.
pub trait Processor: Send {
    fn name(&self) -> &'static str;
    fn prepare(&mut self, sample_rate: u32, max_block_frames: usize);
    fn process(&mut self, block: &mut AudioBlock<'_>);
    fn reset(&mut self);
    fn latency_frames(&self) -> usize {
        0
    }
}

/// A gain node with click-free parameter changes.
pub struct Gain {
    current: f32,
    target: f32,
    smoothing: f32,
}

impl Gain {
    /// `smoothing_ms` is the time constant of the exponential ramp.
    pub fn new(sample_rate: u32, smoothing_ms: f32) -> Self {
        let mut gain = Self {
            current: 1.0,
            target: 1.0,
            smoothing: 0.0,
        };
        gain.set_smoothing(sample_rate, smoothing_ms);
        gain
    }

    fn set_smoothing(&mut self, sample_rate: u32, smoothing_ms: f32) {
        let tau = (smoothing_ms / 1000.0).max(1e-4);
        self.smoothing = 1.0 - (-1.0 / (tau * sample_rate as f32)).exp();
    }

    pub fn set_gain_db(&mut self, db: f32) {
        self.set_target(l3o_core::db_to_linear(db));
    }

    pub fn set_target(&mut self, linear: f32) {
        self.target = linear.clamp(0.0, 16.0);
    }

    pub fn current(&self) -> f32 {
        self.current
    }
}

impl Processor for Gain {
    fn name(&self) -> &'static str {
        "gain"
    }

    fn prepare(&mut self, sample_rate: u32, _max_block_frames: usize) {
        self.set_smoothing(sample_rate, 10.0);
    }

    fn process(&mut self, block: &mut AudioBlock<'_>) {
        if (self.current - self.target).abs() < 1e-6 {
            self.current = self.target;
            if self.current == 1.0 {
                return;
            }
            for sample in block.data.iter_mut() {
                *sample *= self.current;
            }
            return;
        }

        for frame in block.data.chunks_mut(block.channels) {
            self.current += (self.target - self.current) * self.smoothing;
            for sample in frame {
                *sample *= self.current;
            }
        }
    }

    fn reset(&mut self) {
        self.current = self.target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_reaches_target() {
        let mut gain = Gain::new(48_000, 5.0);
        gain.set_target(0.5);
        let mut data = vec![1.0f32; 48_000 * 2];
        let mut block = AudioBlock::new(&mut data, 2, 48_000);
        gain.process(&mut block);
        assert!((gain.current() - 0.5).abs() < 1e-3);
        assert!((data[data.len() - 1] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn unity_gain_is_bit_exact() {
        let mut gain = Gain::new(48_000, 5.0);
        let mut data = vec![0.25f32, -0.25, 0.5, -0.5];
        let original = data.clone();
        let mut block = AudioBlock::new(&mut data, 2, 48_000);
        gain.process(&mut block);
        assert_eq!(data, original);
    }

    #[test]
    fn muted_gain_produces_silence() {
        let mut gain = Gain::new(48_000, 1.0);
        gain.set_target(0.0);
        let mut data = vec![1.0f32; 48_000 * 2];
        let mut block = AudioBlock::new(&mut data, 2, 48_000);
        gain.process(&mut block);
        // The ramp is click-free, so only the tail must be silent.
        let tail = &data[data.len() - 2_000..];
        assert!(tail.iter().all(|s| s.abs() < 1e-4));
    }

    #[test]
    fn block_frame_count() {
        let mut data = vec![0.0f32; 6];
        let block = AudioBlock::new(&mut data, 2, 48_000);
        assert_eq!(block.frames(), 3);
    }
}
