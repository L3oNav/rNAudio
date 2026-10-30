//! Audio device I/O: playback through cpal and capture discovery through
//! flexaudio.
//!
//! Capture is delegated to `flexaudio` (mic, system loopback and per-process).
//! Playback uses cpal 0.16, the same version flexaudio depends on, so only one
//! copy of `alsa-sys` is linked.

pub mod capture;
pub mod output;

pub use capture::{
    CaptureDeviceInfo, ProcessEntry, capabilities_report, list_capture_devices, list_processes,
};
pub use output::{
    OutputDeviceInfo, OutputSummary, describe_output, list_output_devices, play_tone,
};

/// Errors produced by the I/O layer.
#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("no output device is available")]
    NoOutputDevice,

    #[error("output device not found: {0}")]
    DeviceNotFound(String),

    #[error("invalid tone parameter: {0}")]
    InvalidTone(String),

    #[error("device enumeration failed: {0}")]
    Devices(#[from] cpal::DevicesError),

    #[error("could not read the device name: {0}")]
    DeviceName(#[from] cpal::DeviceNameError),

    #[error("could not read the default output configuration: {0}")]
    DefaultConfig(#[from] cpal::DefaultStreamConfigError),

    #[error("could not build the output stream: {0}")]
    BuildStream(#[from] cpal::BuildStreamError),

    #[error("could not start the output stream: {0}")]
    PlayStream(#[from] cpal::PlayStreamError),

    #[error("unsupported sample format: {0}")]
    UnsupportedFormat(String),

    #[error("capture backend error: {0}")]
    Flexaudio(#[from] flexaudio::Error),
}
