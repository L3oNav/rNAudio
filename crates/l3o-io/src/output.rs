//! Playback through cpal 0.16.

use std::f32::consts::TAU;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::IoError;

/// One selectable output device.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputDeviceInfo {
    /// Name used to select the device with cpal.
    pub name: String,
    pub is_default: bool,
    pub channels: u16,
    pub sample_rate: u32,
    pub sample_format: String,
    pub min_buffer_frames: Option<u32>,
    pub max_buffer_frames: Option<u32>,
}

/// Summary used by the `latency` command.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSummary {
    pub name: String,
    pub channels: u16,
    pub sample_rate: u32,
    pub sample_format: String,
    pub min_buffer_frames: Option<u32>,
    pub max_buffer_frames: Option<u32>,
    pub estimated_latency_ms: Option<f32>,
}

/// Lists every output device visible to the default host.
pub fn list_output_devices() -> Result<Vec<OutputDeviceInfo>, IoError> {
    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .and_then(|device| device.name().ok());

    let mut devices = Vec::new();
    for device in host.output_devices()? {
        let name = device.name()?;
        let (channels, sample_rate, sample_format, min_buffer, max_buffer) =
            match device.default_output_config() {
                Ok(config) => {
                    let (min, max) = match config.buffer_size() {
                        cpal::SupportedBufferSize::Range { min, max } => (Some(*min), Some(*max)),
                        cpal::SupportedBufferSize::Unknown => (None, None),
                    };
                    (
                        config.channels(),
                        config.sample_rate().0,
                        format!("{:?}", config.sample_format()),
                        min,
                        max,
                    )
                }
                Err(_) => (0, 0, "unknown".to_string(), None, None),
            };

        devices.push(OutputDeviceInfo {
            is_default: default_name.as_deref() == Some(name.as_str()),
            name,
            channels,
            sample_rate,
            sample_format,
            min_buffer_frames: min_buffer,
            max_buffer_frames: max_buffer,
        });
    }

    Ok(devices)
}

/// Describes the selected output device and estimates its minimum latency.
pub fn describe_output(device_name: Option<&str>) -> Result<OutputSummary, IoError> {
    let device = select_output_device(device_name)?;
    let config = device.default_output_config()?;

    let (min, max) = match config.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => (Some(*min), Some(*max)),
        cpal::SupportedBufferSize::Unknown => (None, None),
    };
    let sample_rate = config.sample_rate().0;
    let estimated_latency_ms = min.map(|frames| frames as f32 / sample_rate as f32 * 1000.0);

    Ok(OutputSummary {
        name: device.name()?,
        channels: config.channels(),
        sample_rate,
        sample_format: format!("{:?}", config.sample_format()),
        min_buffer_frames: min,
        max_buffer_frames: max,
        estimated_latency_ms,
    })
}

/// Plays a sine tone on the selected output device.
///
/// This is the M0 smoke test: it proves that the output path works end to end.
/// The tone fades in and out to avoid clicks.
pub fn play_tone(hz: f32, secs: f32, device_name: Option<&str>) -> Result<(), IoError> {
    if !hz.is_finite() || hz <= 0.0 {
        return Err(IoError::InvalidTone(format!(
            "frequency must be positive, got {hz}"
        )));
    }
    if !secs.is_finite() || secs <= 0.0 {
        return Err(IoError::InvalidTone(format!(
            "duration must be positive, got {secs}"
        )));
    }

    let device = select_output_device(device_name)?;
    let supported = device.default_output_config()?;
    let sample_format = supported.sample_format();
    let config = supported.config();

    let stream = match sample_format {
        SampleFormat::I8 => build_tone::<i8>(&device, &config, hz, secs)?,
        SampleFormat::I16 => build_tone::<i16>(&device, &config, hz, secs)?,
        SampleFormat::I24 => build_tone::<cpal::I24>(&device, &config, hz, secs)?,
        SampleFormat::I32 => build_tone::<i32>(&device, &config, hz, secs)?,
        SampleFormat::I64 => build_tone::<i64>(&device, &config, hz, secs)?,
        SampleFormat::U8 => build_tone::<u8>(&device, &config, hz, secs)?,
        SampleFormat::U16 => build_tone::<u16>(&device, &config, hz, secs)?,
        SampleFormat::U32 => build_tone::<u32>(&device, &config, hz, secs)?,
        SampleFormat::U64 => build_tone::<u64>(&device, &config, hz, secs)?,
        SampleFormat::F32 => build_tone::<f32>(&device, &config, hz, secs)?,
        SampleFormat::F64 => build_tone::<f64>(&device, &config, hz, secs)?,
        other => return Err(IoError::UnsupportedFormat(format!("{other:?}"))),
    };

    stream.play()?;
    // Give the callback a little room so the fade-out is actually rendered.
    std::thread::sleep(Duration::from_secs_f32(secs + 0.1));
    Ok(())
}

fn select_output_device(device_name: Option<&str>) -> Result<cpal::Device, IoError> {
    let host = cpal::default_host();

    match device_name {
        Some(wanted) => host
            .output_devices()?
            .find(|device| device.name().map(|name| name == wanted).unwrap_or(false))
            .ok_or_else(|| IoError::DeviceNotFound(wanted.to_string())),
        None => host.default_output_device().ok_or(IoError::NoOutputDevice),
    }
}

fn build_tone<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    hz: f32,
    secs: f32,
) -> Result<cpal::Stream, IoError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate.0 as f32;
    let nyquist = sample_rate / 2.0;
    let hz = hz.min(nyquist * 0.95);

    let total_frames = (secs * sample_rate).round() as u64;
    let fade_frames = (0.02 * sample_rate).round() as u64;
    let step = TAU * hz / sample_rate;
    let amplitude = 0.2f32;

    let mut phase = 0.0f32;
    let mut written = 0u64;

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            for frame in data.chunks_mut(channels) {
                let sample = if written < total_frames {
                    let envelope = fade_envelope(written, total_frames, fade_frames);
                    let value = phase.sin() * amplitude * envelope;
                    phase = (phase + step) % TAU;
                    written += 1;
                    value
                } else {
                    0.0
                };

                for out in frame {
                    *out = T::from_sample(sample);
                }
            }
        },
        |err| tracing::error!(%err, "output stream error"),
        None,
    )?;

    Ok(stream)
}

/// Linear fade-in/out, expressed in frames.
fn fade_envelope(position: u64, total: u64, fade: u64) -> f32 {
    if fade == 0 || total == 0 {
        return 1.0;
    }
    let from_start = position.min(fade);
    let from_end = total.saturating_sub(position + 1).min(fade);
    (from_start.min(from_end) as f32 / fade as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_fades_in_and_out() {
        assert_eq!(fade_envelope(0, 1000, 100), 0.0);
        assert_eq!(fade_envelope(50, 1000, 100), 0.5);
        assert_eq!(fade_envelope(500, 1000, 100), 1.0);
        assert!((fade_envelope(950, 1000, 100) - 0.49).abs() < 1e-6);
        assert_eq!(fade_envelope(999, 1000, 100), 0.0);
    }

    #[test]
    fn envelope_is_one_without_fade() {
        assert_eq!(fade_envelope(0, 1000, 0), 1.0);
        assert_eq!(fade_envelope(999, 1000, 0), 1.0);
    }

    #[test]
    fn envelope_handles_tiny_signals() {
        assert_eq!(fade_envelope(0, 1, 100), 0.0);
    }
}
