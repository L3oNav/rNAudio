//! Daemon state: owns the engine lifecycle and answers control requests.

use l3o_engine::{Control, ControlSender, Engine};
use l3o_io::DeviceRun;

use crate::ipc::{EngineState, ErrorCode, ErrorReport, Request, StatusReport, validate_gain_db};

/// Everything the daemon owns. Lives behind a mutex; request handling is
/// sequential, so the engine state can never be observed mid-transition.
pub struct Daemon {
    run: Option<DeviceRun>,
    sender: Option<ControlSender>,
    failed: bool,
    block_frames: usize,
}

impl Daemon {
    pub fn new(block_frames: usize) -> Self {
        Self {
            run: None,
            sender: None,
            failed: false,
            block_frames,
        }
    }

    pub fn status(&self) -> StatusReport {
        let engine = if self.run.is_some() {
            EngineState::Running
        } else if self.failed {
            EngineState::Failed
        } else {
            EngineState::Stopped
        };
        let mut report = StatusReport::bare(engine);
        if let Some(run) = &self.run {
            report.stats = Some(run.stats());
            report.sources = Some(run.source_snapshots());
            report.engine_latency_ms = Some(run.engine_latency_ms());
        }
        report
    }

    /// Handles one validated request. Never panics; errors are client-safe.
    pub fn handle(&mut self, request: Request) -> Result<StatusReport, ErrorReport> {
        match request {
            Request::Ping | Request::Status => Ok(self.status()),
            Request::EngineStart => self.start_engine(),
            Request::EngineStop => {
                self.stop_engine();
                Ok(self.status())
            }
            Request::SetBusGain { id, gain_db, muted } => {
                if !validate_gain_db(gain_db) {
                    return Err(err(ErrorCode::InvalidParameter, "gain out of range"));
                }
                self.send(Control::SetBusGain {
                    id: l3o_core::BusId(id),
                    gain_db,
                    muted,
                })
            }
            Request::SetBusSend { id, mix, send } => {
                if !validate_gain_db(send.gain_db) {
                    return Err(err(ErrorCode::InvalidParameter, "send gain out of range"));
                }
                self.send(Control::SetBusSend {
                    id: l3o_core::BusId(id),
                    mix,
                    send,
                })
            }
            Request::SetMaster { master } => {
                if !validate_gain_db(master.monitor_gain_db)
                    || !validate_gain_db(master.stream_gain_db)
                {
                    return Err(err(ErrorCode::InvalidParameter, "master gain out of range"));
                }
                self.send(Control::SetMaster { master })
            }
        }
    }

    fn start_engine(&mut self) -> Result<StatusReport, ErrorReport> {
        if self.run.is_some() {
            return Err(err(ErrorCode::EngineAlreadyRunning, "engine is running"));
        }
        self.failed = false;
        let preset = l3o_core::Preset::default();
        let mut engine = Engine::new(&preset, l3o_core::CANONICAL_SAMPLE_RATE, 2)
            .map_err(|e| err(ErrorCode::EngineStartFailed, e.to_string()))?;
        let sender = engine
            .take_control_sender()
            .expect("fresh engine has a control sender");
        match l3o_io::start_device_run(None, None, None, engine, self.block_frames) {
            Ok(run) => {
                self.run = Some(run);
                self.sender = Some(sender);
                Ok(self.status())
            }
            Err(e) => {
                self.failed = true;
                Err(err(
                    ErrorCode::EngineStartFailed,
                    // The message names the device/API, not user data; still
                    // keep it generic on the wire.
                    format!("audio devices unavailable: {e}"),
                ))
            }
        }
    }

    fn stop_engine(&mut self) {
        self.sender = None;
        self.run = None;
        self.failed = false;
    }

    fn send(&mut self, control: Control) -> Result<StatusReport, ErrorReport> {
        let Some(sender) = self.sender.as_mut() else {
            return Err(err(ErrorCode::EngineNotRunning, "engine is not running"));
        };
        match sender.send(control) {
            Ok(()) => Ok(self.status()),
            Err(l3o_engine::ControlError::QueueFull) => {
                Err(err(ErrorCode::Internal, "control queue is full; retry"))
            }
            Err(l3o_engine::ControlError::Invalid(msg)) => {
                Err(err(ErrorCode::InvalidParameter, msg))
            }
        }
    }
}

fn err(code: ErrorCode, message: impl Into<String>) -> ErrorReport {
    ErrorReport {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::PROTOCOL_VERSION;

    fn daemon() -> Daemon {
        Daemon::new(128)
    }

    #[test]
    fn starts_stopped_and_reports_status() {
        let d = daemon();
        let status = d.status();
        assert_eq!(status.engine, EngineState::Stopped);
        assert_eq!(status.protocol_version, PROTOCOL_VERSION);
    }

    #[test]
    fn rejects_controls_when_stopped() {
        let mut d = daemon();
        let result = d.handle(Request::SetBusGain {
            id: 0,
            gain_db: -3.0,
            muted: false,
        });
        assert!(matches!(
            result,
            Err(ErrorReport {
                code: ErrorCode::EngineNotRunning,
                ..
            })
        ));
    }

    #[test]
    fn rejects_invalid_gains_without_touching_state() {
        let mut d = daemon();
        for bad in [f32::NAN, 500.0, f32::NEG_INFINITY] {
            let result = d.handle(Request::SetBusGain {
                id: 0,
                gain_db: bad,
                muted: false,
            });
            assert!(matches!(
                result,
                Err(ErrorReport {
                    code: ErrorCode::InvalidParameter,
                    ..
                })
            ));
        }
        assert_eq!(d.status().engine, EngineState::Stopped);
    }

    #[test]
    fn stop_when_stopped_is_a_clean_noop() {
        let mut d = daemon();
        let status = d.handle(Request::EngineStop).unwrap();
        assert_eq!(status.engine, EngineState::Stopped);
    }

    // EngineStart on hardware-less CI returns EngineStartFailed and moves the
    // state to Failed; covered by the ipc integration test against a real
    // socket rather than unit-tested here.
}
