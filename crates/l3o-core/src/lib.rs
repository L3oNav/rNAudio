//! Core domain model for L3O Audio.
//!
//! This crate is deliberately free of platform and audio-API code so it can be
//! tested on every target and reused by the daemon, the GUI and the CLI.

use serde::{Deserialize, Serialize};

pub mod gain;

pub use gain::{db_to_linear, linear_to_db};

/// Everything inside the engine runs at this rate. Device rates are converted
/// at the I/O boundary.
pub const CANONICAL_SAMPLE_RATE: u32 = 48_000;

/// Stereo for v1. Multichannel layouts come later.
pub const MAX_CHANNELS: usize = 2;

/// Stable identifier for a mixer channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BusId(pub u32);

/// The two output mixes of Streamer Mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MixId {
    /// What the operator hears (headphones/speakers).
    Monitor,
    /// What the audience hears (OBS / capture software).
    Stream,
}

impl MixId {
    pub const ALL: [MixId; 2] = [MixId::Monitor, MixId::Stream];

    pub fn label(self) -> &'static str {
        match self {
            MixId::Monitor => "Monitor",
            MixId::Stream => "Stream",
        }
    }
}

/// Routing of one bus into one mix.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Send {
    pub enabled: bool,
    pub gain_db: f32,
}

impl Send {
    pub const UNITY: Self = Self {
        enabled: true,
        gain_db: 0.0,
    };

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            gain_db: 0.0,
        }
    }

    pub fn linear_gain(self) -> f32 {
        if self.enabled {
            db_to_linear(self.gain_db)
        } else {
            0.0
        }
    }
}

impl Default for Send {
    fn default() -> Self {
        Self::UNITY
    }
}

/// Per-bus send configuration for both mixes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sends {
    pub monitor: Send,
    pub stream: Send,
}

impl Default for Sends {
    fn default() -> Self {
        Self {
            monitor: Send::UNITY,
            stream: Send::UNITY,
        }
    }
}

impl Sends {
    pub fn send(&self, mix: MixId) -> Send {
        match mix {
            MixId::Monitor => self.monitor,
            MixId::Stream => self.stream,
        }
    }

    pub fn send_mut(&mut self, mix: MixId) -> &mut Send {
        match mix {
            MixId::Monitor => &mut self.monitor,
            MixId::Stream => &mut self.stream,
        }
    }
}

/// One mixer channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bus {
    pub id: BusId,
    pub name: String,
    pub gain_db: f32,
    pub muted: bool,
    pub sends: Sends,
}

impl Bus {
    pub fn new(id: u32, name: impl Into<String>) -> Self {
        Self {
            id: BusId(id),
            name: name.into(),
            gain_db: 0.0,
            muted: false,
            sends: Sends::default(),
        }
    }

    pub fn linear_gain(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            db_to_linear(self.gain_db)
        }
    }
}

/// Master settings for both mixes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Master {
    pub monitor_gain_db: f32,
    pub stream_gain_db: f32,
    pub monitor_muted: bool,
    pub stream_muted: bool,
}

impl Default for Master {
    fn default() -> Self {
        Self {
            monitor_gain_db: 0.0,
            stream_gain_db: 0.0,
            monitor_muted: false,
            stream_muted: false,
        }
    }
}

impl Master {
    pub fn linear_gain(&self, mix: MixId) -> f32 {
        let (db, muted) = match mix {
            MixId::Monitor => (self.monitor_gain_db, self.monitor_muted),
            MixId::Stream => (self.stream_gain_db, self.stream_muted),
        };
        if muted { 0.0 } else { db_to_linear(db) }
    }
}

/// A complete mixer state that can be persisted and restored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub buses: Vec<Bus>,
    pub master: Master,
}

impl Preset {
    /// The default channel layout, modelled after Sonar's Streamer Mode.
    pub fn default_layout() -> Self {
        Self {
            name: "Default".to_string(),
            buses: vec![
                Bus::new(0, "Game"),
                Bus::new(1, "Chat"),
                Bus::new(2, "Media"),
                Bus::new(3, "Aux"),
                Bus::new(4, "Mic"),
                Bus::new(5, "System"),
            ],
            master: Master::default(),
        }
    }

    pub fn bus(&self, id: BusId) -> Option<&Bus> {
        self.buses.iter().find(|bus| bus.id == id)
    }

    pub fn bus_mut(&mut self, id: BusId) -> Option<&mut Bus> {
        self.buses.iter_mut().find(|bus| bus.id == id)
    }
}

impl Default for Preset {
    fn default() -> Self {
        Self::default_layout()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_has_sonar_channels() {
        let preset = Preset::default();
        let names: Vec<_> = preset.buses.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["Game", "Chat", "Media", "Aux", "Mic", "System"]);
    }

    #[test]
    fn muted_bus_is_silent() {
        let mut bus = Bus::new(0, "Game");
        bus.muted = true;
        assert_eq!(bus.linear_gain(), 0.0);
    }

    #[test]
    fn disabling_a_send_zeroes_its_gain() {
        let mut sends = Sends::default();
        sends.stream.enabled = false;
        assert_eq!(sends.send(MixId::Stream).linear_gain(), 0.0);
        assert!(sends.send(MixId::Monitor).linear_gain() > 0.0);
    }

    #[test]
    fn master_mute_applies_per_mix() {
        let master = Master {
            stream_muted: true,
            ..Master::default()
        };
        assert_eq!(master.linear_gain(MixId::Stream), 0.0);
        assert_eq!(master.linear_gain(MixId::Monitor), 1.0);
    }

    #[test]
    fn preset_lookup_works() {
        let mut preset = Preset::default();
        assert_eq!(preset.bus(BusId(2)).unwrap().name, "Media");
        preset.bus_mut(BusId(2)).unwrap().name = "Music".into();
        assert_eq!(preset.bus(BusId(2)).unwrap().name, "Music");
    }
}
