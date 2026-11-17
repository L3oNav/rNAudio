//! Control handoff from non-RT threads to the engine.
//!
//! Design (see `docs/rt-design.md`): bounded SPSC queue, validated before
//! enqueue, drained once per block by the render thread.

use l3o_core::{BusId, Master, MixId, Send};

/// Capacity of the control queue. Full is a reported error, never a block.
pub const CONTROL_QUEUE_CAPACITY: usize = 64;

/// One complete control change. Messages are values, so the engine applies
/// each one atomically at a block boundary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Control {
    /// Set a bus channel gain and mute.
    SetBusGain {
        id: BusId,
        gain_db: f32,
        muted: bool,
    },
    /// Set one bus send (gain + enabled).
    SetBusSend { id: BusId, mix: MixId, send: Send },
    /// Replace the whole master section.
    SetMaster { master: Master },
}

/// Why a control message was rejected before reaching the engine.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ControlError {
    #[error("queue is full; retry or drop")]
    QueueFull,

    #[error("invalid control: {0}")]
    Invalid(String),
}

/// Producer side of the control queue. Held by any non-RT thread.
pub struct ControlSender {
    inner: rtrb::Producer<Control>,
}

impl ControlSender {
    /// Validate then enqueue a control message.
    pub fn send(&mut self, control: Control) -> Result<(), ControlError> {
        validate(&control)?;
        self.inner
            .push(control)
            .map_err(|_| ControlError::QueueFull)
    }

    /// Number of messages currently waiting for the engine.
    pub fn pending(&self) -> usize {
        self.inner.slots()
    }
}

pub(crate) struct ControlReceiver {
    inner: rtrb::Consumer<Control>,
}

impl ControlReceiver {
    pub(crate) fn pop(&mut self) -> Option<Control> {
        self.inner.pop().ok()
    }
}

/// Creates the control queue endpoints. The engine keeps the receiver; the
/// caller hands the sender to whoever changes the mix.
pub(crate) fn control_queue() -> (ControlSender, ControlReceiver) {
    let (producer, consumer) = rtrb::RingBuffer::new(CONTROL_QUEUE_CAPACITY);
    (
        ControlSender { inner: producer },
        ControlReceiver { inner: consumer },
    )
}

fn validate(control: &Control) -> Result<(), ControlError> {
    let check = |what: &str, db: f32| -> Result<(), ControlError> {
        if db.is_finite() && (l3o_core::MIN_GAIN_DB..=l3o_core::MAX_GAIN_DB).contains(&db) {
            Ok(())
        } else {
            Err(ControlError::Invalid(format!("{what}: {db}")))
        }
    };
    match control {
        Control::SetBusGain { gain_db, .. } => check("bus gain", *gain_db),
        Control::SetBusSend { send, .. } => check("send gain", send.gain_db),
        Control::SetMaster { master } => {
            check("master monitor gain", master.monitor_gain_db)?;
            check("master stream gain", master.stream_gain_db)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use l3o_core::{Master, Send};

    #[test]
    fn send_validates_before_enqueue() {
        let (mut sender, mut receiver) = control_queue();
        let bad = Control::SetBusGain {
            id: BusId(0),
            gain_db: f32::NAN,
            muted: false,
        };
        assert!(matches!(sender.send(bad), Err(ControlError::Invalid(_))));
        assert!(receiver.pop().is_none());

        let out_of_range = Control::SetBusSend {
            id: BusId(0),
            mix: MixId::Monitor,
            send: Send {
                enabled: true,
                gain_db: 100.0,
            },
        };
        assert!(matches!(
            sender.send(out_of_range),
            Err(ControlError::Invalid(_))
        ));
    }

    #[test]
    fn full_queue_is_an_error_not_a_block() {
        let (mut sender, _receiver) = control_queue();
        for i in 0..CONTROL_QUEUE_CAPACITY {
            sender
                .send(Control::SetBusGain {
                    id: BusId(i as u32),
                    gain_db: 0.0,
                    muted: false,
                })
                .unwrap();
        }
        assert!(matches!(
            sender.send(Control::SetMaster {
                master: Master::default(),
            }),
            Err(ControlError::QueueFull)
        ));
    }

    #[test]
    fn valid_controls_pass_through() {
        let (mut sender, mut receiver) = control_queue();
        sender
            .send(Control::SetBusGain {
                id: BusId(3),
                gain_db: -6.0,
                muted: true,
            })
            .unwrap();
        let got = receiver.pop().unwrap();
        assert!(matches!(
            got,
            Control::SetBusGain {
                id: BusId(3),
                gain_db: -6.0,
                muted: true
            }
        ));
    }
}
