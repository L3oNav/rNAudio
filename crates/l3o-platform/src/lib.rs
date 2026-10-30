//! Platform-specific virtual audio devices and application routing.
//!
//! M0 only provides detection and the trait that the real backends will
//! implement:
//!
//! * Linux: PipeWire null sinks per bus (no driver required).
//! * macOS: process taps with `CATapMuteBehavior::mutedWhenTapped`, plus an
//!   optional HAL plug-in for the Stream output.
//! * Windows: WASAPI process loopback first; a signed driver or third-party
//!   virtual cable later.

use l3o_core::{Bus, BusId};

/// Which platform backend is available in the current session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Linux with a reachable PipeWire daemon.
    PipeWire,
    /// Windows; implementation lands in M7.
    Windows,
    /// macOS; implementation lands in M6.
    CoreAudio,
    /// No supported backend detected.
    Unsupported,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::PipeWire => "pipewire",
            Backend::Windows => "windows",
            Backend::CoreAudio => "coreaudio",
            Backend::Unsupported => "unsupported",
        }
    }

    pub fn is_supported(self) -> bool {
        !matches!(self, Backend::Unsupported)
    }
}

/// A virtual device pair belonging to one bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusDevice {
    /// Node the applications should play into.
    pub sink: String,
    /// Node the engine captures (the sink monitor).
    pub monitor: String,
    pub bus: BusId,
}

/// Manages the virtual devices the mixer needs.
pub trait VirtualDevices {
    fn backend(&self) -> Backend;
    fn ensure_bus_device(&mut self, bus: &Bus) -> Result<BusDevice, PlatformError>;
    fn remove_bus_device(&mut self, bus: BusId) -> Result<(), PlatformError>;
    fn remove_all(&mut self) -> Result<(), PlatformError>;
}

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("no supported platform backend is available")]
    Unsupported,
    #[error("failed to talk to the audio server: {0}")]
    Backend(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Detects the best available backend without opening any audio device.
pub fn detect() -> Backend {
    #[cfg(target_os = "linux")]
    {
        if linux_pipewire_running() {
            Backend::PipeWire
        } else {
            Backend::Unsupported
        }
    }

    #[cfg(target_os = "windows")]
    {
        Backend::Windows
    }

    #[cfg(target_os = "macos")]
    {
        Backend::CoreAudio
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Backend::Unsupported
    }
}

#[cfg(target_os = "linux")]
fn linux_pipewire_running() -> bool {
    use std::path::PathBuf;

    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR")
        && PathBuf::from(runtime_dir).join("pipewire-0").exists()
    {
        return true;
    }

    // Fall back to the default per-user socket path.
    let uid = unsafe { libc_getuid() };
    PathBuf::from(format!("/run/user/{uid}/pipewire-0")).exists()
}

// Avoid pulling in the `libc` crate for a single syscall.
#[cfg(target_os = "linux")]
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: `getuid` has no preconditions and cannot fail.
    unsafe { getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_names_are_stable() {
        assert_eq!(Backend::PipeWire.name(), "pipewire");
        assert_eq!(Backend::Unsupported.name(), "unsupported");
        assert!(!Backend::Unsupported.is_supported());
    }

    #[test]
    fn detection_never_panics() {
        let backend = detect();
        assert!(backend.is_supported() || backend == Backend::Unsupported);
    }
}
