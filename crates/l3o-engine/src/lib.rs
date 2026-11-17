//! The mixer graph: buses feed two independent mixes (Monitor and Stream).
//!
//! The engine is owned by the render thread; controls arrive over a bounded
//! queue and every amplitude change is click-free (see `docs/rt-design.md`).

mod control;
mod engine;

pub use control::{CONTROL_QUEUE_CAPACITY, Control, ControlError, ControlSender};
pub use engine::{Engine, EngineError, MixAccumulator};
