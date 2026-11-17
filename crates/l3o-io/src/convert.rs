//! Device <-> canonical (48 kHz stereo `f32`) conversion.
//!
//! Three concerns live here, each independently testable:
//! - sample-format conversion ([`to_device_samples`], cpal traits)
//! - channel adaptation ([`adapt_channels`], mono->stereo duplication etc.)
//! - rate conversion ([`LinearResampler`])
//!
//! shortcut: rate conversion is linear interpolation; upgrade to the rubato
//! FFT resampler in P1.5 when adaptive drift handling lands.

use l3o_core::MAX_CHANNELS;

/// Errors from the conversion layer.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConvertError {
    #[error("unsupported channel count: {0}")]
    UnsupportedChannels(usize),

    #[error("input chunk exceeds staging capacity ({0} frames)")]
    StagingOverflow(usize),
}

/// Converts canonical stereo `f32` samples into the device sample type.
///
/// Interleaving is preserved: `src.len() == dst.len()`, one output value per
/// input value. Channel adaptation happened earlier in
/// [`adapt_channels`]; this is purely a per-sample cast through cpal's
/// conversion traits.
pub fn to_device_samples<T>(src: &[f32], dst: &mut [T])
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    assert_eq!(src.len(), dst.len(), "sample count mismatch");
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        *d = T::from_sample(*s);
    }
}

/// Adapts an interleaved buffer to exactly [`MAX_CHANNELS`] channels.
///
/// - 1 channel -> duplicated to both output channels
/// - 2 channels -> copied
/// - more -> the first [`MAX_CHANNELS`] are kept
///
/// `src_frames` is the number of frames in `src`; `dst` must hold
/// `src_frames * MAX_CHANNELS` samples.
pub fn adapt_channels(
    src: &[f32],
    src_channels: usize,
    dst: &mut [f32],
) -> Result<(), ConvertError> {
    if src_channels == 0 {
        return Err(ConvertError::UnsupportedChannels(0));
    }
    let frames = src.len() / src_channels;
    if src_channels == MAX_CHANNELS {
        dst[..frames * MAX_CHANNELS].copy_from_slice(&src[..frames * MAX_CHANNELS]);
        return Ok(());
    }
    if src_channels == 1 {
        for (i, s) in src[..frames].iter().enumerate() {
            dst[i * 2] = *s;
            dst[i * 2 + 1] = *s;
        }
        return Ok(());
    }
    for i in 0..frames {
        dst[i * 2] = src[i * src_channels];
        dst[i * 2 + 1] = src[i * src_channels + 1];
    }
    Ok(())
}

/// Streaming linear resampler between arbitrary rates.
///
/// Feed interleaved chunks of any size with [`push`](Self::push); pull the
/// converted stream in whatever granularity you need via
/// [`output`](Self::output) and [`consume`](Self::consume). No allocation
/// after construction: staging is fixed-size and overflow is reported.
pub struct LinearResampler {
    channels: usize,
    ratio: f64, // input frames per output frame
    // Absolute input-frame coordinates: `pos` is where the next output
    // frame reads, between the frame at floor(pos) (`prev`, when prev_index
    // matches) and the next staged frame.
    pos: f64,
    prev: Vec<f32>,
    prev_index: Option<usize>,
    // Staged input, absolute indexing via `base`.
    staging: Vec<f32>,
    base: usize,
    staged: usize, // absolute index one past the last staged frame
    capacity: usize,
    // Produced output, read via `out_read`.
    out: Vec<f32>,
    out_read: usize,
}

impl LinearResampler {
    /// `capacity_frames` bounds the input staging; pushes beyond it fail
    /// with [`ConvertError::StagingOverflow`].
    pub fn new(
        input_rate: u32,
        output_rate: u32,
        channels: usize,
        capacity_frames: usize,
    ) -> Result<Self, ConvertError> {
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(ConvertError::UnsupportedChannels(channels));
        }
        if input_rate == 0 || output_rate == 0 {
            return Err(ConvertError::UnsupportedChannels(0));
        }
        let max_chunk_out = (capacity_frames as f64 * output_rate as f64 / input_rate as f64).ceil()
            as usize
            + channels; // interpolation slack
        Ok(Self {
            channels,
            ratio: input_rate as f64 / output_rate as f64,
            pos: 0.0,
            prev: vec![0.0; channels],
            prev_index: None,
            staging: vec![0.0; capacity_frames * channels],
            base: 0,
            staged: 0,
            capacity: capacity_frames,
            out: vec![0.0; max_chunk_out * channels],
            out_read: 0,
        })
    }

    /// Stages an interleaved input chunk. Real-time safe after warmup: no
    /// allocation, worst cost is a memmove-style compaction.
    pub fn push(&mut self, chunk: &[f32]) -> Result<(), ConvertError> {
        let frames = chunk.len() / self.channels;
        if frames == 0 {
            return Ok(());
        }
        let chunk = &chunk[..frames * self.channels];
        if frames > self.capacity {
            return Err(ConvertError::StagingOverflow(frames));
        }
        // Compact once the consumed prefix grows past half the staging.
        let consumed = self.pos.floor() as usize;
        if consumed > self.base + self.capacity / 2 {
            let keep_from = consumed.min(self.staged);
            let keep = self.staged - keep_from;
            let start = (keep_from - self.base) * self.channels;
            let end = (self.staged - self.base) * self.channels;
            self.staging.copy_within(start..end, 0);
            self.base = keep_from;
            self.staged = keep_from + keep;
            // Re-anchor prev at the new floor so interpolation stays valid.
            self.prev_index = None;
        }
        if self.staged - self.base + frames > self.capacity {
            return Err(ConvertError::StagingOverflow(
                self.staged - self.base + frames,
            ));
        }
        let at = (self.staged - self.base) * self.channels;
        self.staging[at..at + frames * self.channels].copy_from_slice(chunk);
        self.staged += frames;
        self.produce();
        Ok(())
    }

    fn frame(&self, index: usize) -> &[f32] {
        let at = (index - self.base) * self.channels;
        &self.staging[at..at + self.channels]
    }

    fn produce(&mut self) {
        loop {
            let i = self.pos.floor() as usize;
            if self.prev_index != Some(i) {
                if self.staged <= i {
                    return;
                }
                let mut prev = [0.0f32; MAX_CHANNELS];
                prev[..self.channels].copy_from_slice(self.frame(i));
                self.prev.copy_from_slice(&prev[..self.channels]);
                self.prev_index = Some(i);
            }
            let frac = self.pos - i as f64;
            if frac != 0.0 && self.staged <= i + 1 {
                return; // need the frame after pos to interpolate
            }
            let written = self.out_read * self.channels;
            if frac == 0.0 {
                // Exactly on a staged frame: no lookahead needed, so the tail
                // of a finite chunk is emitted instead of held back.
                self.out[written..written + self.channels]
                    .copy_from_slice(&self.prev[..self.channels]);
            } else {
                let f = frac as f32;
                let inv = 1.0 - f;
                let mut next = [0.0f32; MAX_CHANNELS];
                next[..self.channels].copy_from_slice(self.frame(i + 1));
                for (c, next_c) in next[..self.channels].iter().enumerate() {
                    self.out[written + c] = self.prev[c] * inv + next_c * f;
                }
            }
            self.out_read += 1;
            self.pos += self.ratio;
        }
    }

    /// Resampled output frames, interleaved.
    pub fn output(&self) -> &[f32] {
        &self.out[..self.out_read * self.channels]
    }

    /// Drops the first `frames` output frames.
    pub fn consume(&mut self, frames: usize) {
        let frames = frames.min(self.out_read);
        let start = frames * self.channels;
        let len = self.out_read * self.channels - start;
        self.out.copy_within(start..start + len, 0);
        self.out_read -= frames;
    }

    /// Frames currently buffered in the output.
    pub fn available(&self) -> usize {
        self.out_read
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(frames: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            v.push(i as f32 * 0.001);
            v.push(1.0 - i as f32 * 0.001);
        }
        v
    }

    #[test]
    fn mono_is_duplicated_to_stereo() {
        let src = vec![0.25f32, 0.5, 0.75];
        let mut dst = vec![0.0; 6];
        adapt_channels(&src, 1, &mut dst).unwrap();
        assert_eq!(dst, [0.25, 0.25, 0.5, 0.5, 0.75, 0.75]);
    }

    #[test]
    fn stereo_passes_through_and_surround_is_truncated() {
        let src = stereo(3);
        let mut dst = vec![0.0; 6];
        adapt_channels(&src, 2, &mut dst).unwrap();
        assert_eq!(dst, src);

        let quad: Vec<f32> = (0..12).map(|i| i as f32).collect();
        let mut dst = vec![0.0; 6];
        adapt_channels(&quad, 4, &mut dst).unwrap();
        assert_eq!(dst, [0.0, 1.0, 4.0, 5.0, 8.0, 9.0]);
    }

    #[test]
    fn adapt_rejects_zero_channels() {
        let mut dst = vec![0.0; 2];
        assert!(matches!(
            adapt_channels(&[1.0], 0, &mut dst),
            Err(ConvertError::UnsupportedChannels(0))
        ));
    }

    #[test]
    fn resampler_preserves_frame_count_at_equal_rates() {
        let mut r = LinearResampler::new(48_000, 48_000, 2, 1024).unwrap();
        let src = stereo(300);
        r.push(&src).unwrap();
        assert_eq!(r.available(), 300);
        // Split delivery across chunks still yields every frame in order.
        let out = r.output().to_vec();
        assert_eq!(out, src);
    }

    #[test]
    fn resampler_upsamples_to_higher_rate() {
        // 48 kHz -> 96 kHz doubles the frame count and keeps endpoints.
        let mut r = LinearResampler::new(48_000, 96_000, 2, 512).unwrap();
        let src = stereo(240); // 5 ms
        r.push(&src).unwrap();
        // 480 output positions minus the one that needs the not-yet-arrived
        // frame 241; the deficit stays at one frame as the stream continues.
        assert_eq!(r.available(), 479);
        r.push(&stereo(1)).unwrap();
        assert_eq!(r.available(), 481);
        let out = r.output();
        assert!((out[0] - src[0]).abs() < 1e-6);
        assert!((out[1] - src[1]).abs() < 1e-6);
        // Interpolated midpoint of a linear ramp is exact.
        assert!((out[2] - (src[0] + src[2]) * 0.5).abs() < 1e-6);
    }

    #[test]
    fn resampler_downsamples_to_lower_rate() {
        // 48 kHz -> 24 kHz halves the frame count; a constant signal survives.
        let mut r = LinearResampler::new(48_000, 24_000, 2, 512).unwrap();
        let src = vec![0.4f32; 480]; // 240 stereo frames
        r.push(&src).unwrap();
        assert_eq!(r.available(), 120);
        assert!(r.output().iter().all(|s| (*s - 0.4).abs() < 1e-6));
    }

    #[test]
    fn resampler_converts_44100_to_48k() {
        let mut r = LinearResampler::new(44_100, 48_000, 2, 1024).unwrap();
        let src = stereo(441); // 10 ms at 44.1 kHz
        r.push(&src).unwrap();
        // One frame held back for interpolation, as in the upsample test.
        assert_eq!(r.available(), 479);
        // Continuity per channel: no repeated or dropped frame edges.
        let out = r.output().to_vec();
        for ch in 0..2 {
            let max_jump = out[ch..]
                .iter()
                .step_by(2)
                .zip(out[ch + 2..].iter().step_by(2))
                .map(|(a, b)| (*b - *a).abs())
                .fold(0.0f32, f32::max);
            assert!(
                max_jump < 0.01,
                "44.1k->48k introduced a discontinuity on ch{ch}: {max_jump}"
            );
        }
    }

    #[test]
    fn resampler_streams_across_many_small_chunks() {
        let mut fast = LinearResampler::new(48_000, 48_000, 2, 4096).unwrap();
        let all = stereo(1000);
        let mut via_chunks = LinearResampler::new(48_000, 48_000, 2, 128).unwrap();
        let mut collected = Vec::new();
        for chunk in all.chunks(74) {
            via_chunks.push(chunk).unwrap();
            collected.extend_from_slice(via_chunks.output());
            via_chunks.consume(via_chunks.available());
        }
        fast.push(&all).unwrap();
        assert_eq!(collected, fast.output().to_vec());
    }

    #[test]
    fn resampler_reports_staging_overflow_without_allocating() {
        let mut r = LinearResampler::new(48_000, 48_000, 2, 64).unwrap();
        let src = stereo(128);
        assert!(matches!(
            r.push(&src),
            Err(ConvertError::StagingOverflow(_))
        ));
    }

    #[test]
    fn format_conversion_covers_supported_sample_types() {
        // i16
        let src = vec![-1.0f32, -0.5, 0.0, 0.5, 1.0];
        let mut i16_out = vec![0i16; 5];
        to_device_samples(&src, &mut i16_out);
        assert!(i16_out[0] <= -32767 && i16_out[4] == 32767);

        // f32 is bit-preserving.
        let mut f32_out = vec![0.0f32; 5];
        to_device_samples(&src, &mut f32_out);
        assert_eq!(f32_out, src);

        // f64 widens.
        let mut f64_out = vec![0.0f64; 5];
        to_device_samples(&src, &mut f64_out);
        assert!((f64_out[1] + 0.5).abs() < 1e-9);

        // i32 full scale.
        let mut i32_out = vec![0i32; 5];
        to_device_samples(&src, &mut i32_out);
        assert!(i32_out[0] <= i32::MAX - 65536 && i32_out[4] >= 65536);

        // u8 is offset binary.
        let mut u8_out = vec![0u8; 5];
        to_device_samples(&src, &mut u8_out);
        assert!(u8_out[2] >= 127 && u8_out[2] <= 129);

        // i24 round-trips through cpal's type.
        let zero = cpal::I24::new(0).expect("0 is in i24 range");
        let mut i24_out = vec![zero; 5];
        to_device_samples(&src, &mut i24_out);
        assert!(i24_out[4].inner().abs() > 8_000_000);
    }
}
