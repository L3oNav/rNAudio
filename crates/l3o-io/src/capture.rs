//! Capture discovery through flexaudio.

use crate::IoError;

/// One selectable capture device (microphone or system-output monitor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDeviceInfo {
    /// Value accepted by `StreamConfig::device_id`.
    pub id: String,
    pub name: String,
    /// `mic` or `system`.
    pub kind: &'static str,
    pub is_loopback: bool,
    pub is_default: bool,
    pub sample_rate: u32,
    pub channels: u16,
    pub format_available: bool,
}

/// One process that currently owns an audio output session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub name: String,
    pub executable: Option<String>,
    pub bundle_id: Option<String>,
    pub is_output_active: Option<bool>,
}

/// Lists every microphone and system-output endpoint.
pub fn list_capture_devices() -> Result<Vec<CaptureDeviceInfo>, IoError> {
    let devices = flexaudio::devices()?;
    Ok(devices
        .into_iter()
        .map(|device| CaptureDeviceInfo {
            id: device.id,
            name: device.name,
            kind: match device.source_kind {
                flexaudio::SourceKind::Mic => "mic",
                _ => "system",
            },
            is_loopback: device.is_loopback,
            is_default: device.is_default,
            sample_rate: device.sample_rate,
            channels: device.channels,
            format_available: device.format_available,
        })
        .collect())
}

/// Lists processes with an audio output session (candidates for per-process capture).
pub fn list_processes() -> Result<Vec<ProcessEntry>, IoError> {
    let processes = flexaudio::processes()?;
    Ok(processes
        .into_iter()
        .map(|process| ProcessEntry {
            pid: process.pid,
            name: process.name,
            executable: process.executable,
            bundle_id: process.bundle_id,
            is_output_active: process.is_output_active,
        })
        .collect())
}

/// Returns flexaudio's capability snapshot as a human-readable report.
///
/// The snapshot opens no capture streams and requests no permissions.
pub fn capabilities_report() -> String {
    format!("{:#?}", flexaudio::capabilities())
}
