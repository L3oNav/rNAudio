//! Versioned local IPC protocol between `l3od` and its clients.
//!
//! One JSON object per line over a user-session Unix socket. Every request
//! carries a protocol version and sequence number; every response is bounded
//! by [`MAX_FRAME_BYTES`]. Malformed input is rejected before it reaches
//! daemon state.

use serde::{Deserialize, Serialize};

/// Bumped on every breaking protocol change. There is no compatibility
/// promise across versions yet (P1.4 limit).
pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum request/response size. Frames beyond this are rejected and the
/// connection closes; no unbounded read exists.
pub const MAX_FRAME_BYTES: usize = 4096;

/// How long a client may stay silent mid-request before the server gives up.
pub const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// One control operation. `serde(tag)` keeps the wire format explicit and
/// rejects unknown operations at parse time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Status,
    EngineStart,
    EngineStop,
    SetBusGain {
        id: u32,
        gain_db: f32,
        muted: bool,
    },
    SetBusSend {
        id: u32,
        mix: l3o_core::MixId,
        send: l3o_core::Send,
    },
    SetMaster {
        master: l3o_core::Master,
    },
}

/// Engine state as reported to clients. Carries no device names or paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineState {
    Stopped,
    Running,
    Failed,
}

/// Response body for `Status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusReport {
    pub engine: EngineState,
    pub protocol_version: u32,
    /// Present while the engine runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<l3o_io::RenderStatsSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<l3o_io::SourceSnapshot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_latency_ms: Option<f32>,
}

impl StatusReport {
    pub fn bare(engine: EngineState) -> Self {
        Self {
            engine,
            protocol_version: PROTOCOL_VERSION,
            stats: None,
            sources: None,
            engine_latency_ms: None,
        }
    }
}

/// What the daemon failed to do, in client-safe terms (no internals).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorReport {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    MalformedRequest,
    UnsupportedVersion,
    EngineNotRunning,
    EngineAlreadyRunning,
    EngineStartFailed,
    InvalidParameter,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestFrame {
    pub version: u32,
    pub seq: u64,
    #[serde(flatten)]
    pub request: Request,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseFrame {
    pub version: u32,
    pub seq: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorReport>,
}

/// Errors the server can hit before a request exists. Converted into an
/// error frame where possible, else the connection just closes.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WireError {
    #[error("frame exceeds {MAX_FRAME_BYTES} bytes")]
    FrameTooLarge,

    #[error("frame is not valid protocol version {PROTOCOL_VERSION} JSON")]
    Malformed,
}

/// Parses one request frame, enforcing size and version before anything else.
pub fn parse_request(raw: &[u8]) -> Result<RequestFrame, WireError> {
    if raw.len() > MAX_FRAME_BYTES {
        return Err(WireError::FrameTooLarge);
    }
    let frame: RequestFrame = serde_json::from_slice(raw).map_err(|_| WireError::Malformed)?;
    if frame.version != PROTOCOL_VERSION {
        return Err(WireError::Malformed);
    }
    Ok(frame)
}

/// Validates gain values that serde cannot check (finite, in range).
pub fn validate_gain_db(db: f32) -> bool {
    db.is_finite() && (l3o_core::MIN_GAIN_DB..=l3o_core::MAX_GAIN_DB).contains(&db)
}

/// Serializes a response, refusing to produce an oversized frame.
pub fn write_response(frame: &ResponseFrame) -> Result<Vec<u8>, WireError> {
    let mut raw = serde_json::to_vec(frame).map_err(|_| WireError::Malformed)?;
    if raw.len() > MAX_FRAME_BYTES {
        return Err(WireError::FrameTooLarge);
    }
    raw.push(b'\n');
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use l3o_core::{Master, MixId, Send};

    fn frame(request: Request) -> RequestFrame {
        RequestFrame {
            version: PROTOCOL_VERSION,
            seq: 7,
            request,
        }
    }

    #[test]
    fn round_trips_versioned_frames() {
        let frame = frame(Request::SetBusGain {
            id: 2,
            gain_db: -6.0,
            muted: true,
        });
        let raw = serde_json::to_vec(&frame).unwrap();
        let parsed = parse_request(&raw).unwrap();
        assert_eq!(parsed, frame);
        assert_eq!(parsed.seq, 7);
    }

    #[test]
    fn all_operations_round_trip() {
        let ops = [
            Request::Ping,
            Request::Status,
            Request::EngineStart,
            Request::EngineStop,
            Request::SetBusSend {
                id: 0,
                mix: MixId::Stream,
                send: Send::disabled(),
            },
            Request::SetMaster {
                master: Master::default(),
            },
        ];
        for op in ops {
            let parsed = parse_request(&serde_json::to_vec(&frame(op.clone())).unwrap()).unwrap();
            assert_eq!(parsed.request, op);
        }
    }

    #[test]
    fn rejects_wrong_version_garbage_and_oversize() {
        let wrong_version = br#"{"version":99,"seq":1,"op":"ping"}"#;
        assert!(matches!(
            parse_request(wrong_version),
            Err(WireError::Malformed)
        ));
        assert!(matches!(
            parse_request(b"not json at all"),
            Err(WireError::Malformed)
        ));
        assert!(matches!(
            parse_request(&vec![b'{'; MAX_FRAME_BYTES + 1]),
            Err(WireError::FrameTooLarge)
        ));
        // Unknown op is a schema error at parse time.
        let unknown_op = br#"{"version":1,"seq":1,"op":"delete_everything"}"#;
        assert!(matches!(
            parse_request(unknown_op),
            Err(WireError::Malformed)
        ));
    }

    #[test]
    fn non_finite_gains_fail_validation() {
        assert!(!validate_gain_db(f32::NAN));
        assert!(!validate_gain_db(f32::INFINITY));
        assert!(!validate_gain_db(120.0));
        assert!(validate_gain_db(-144.0));
        assert!(validate_gain_db(24.0));
        assert!(validate_gain_db(0.0));
    }

    #[test]
    fn responses_are_bounded() {
        let big = ResponseFrame {
            version: PROTOCOL_VERSION,
            seq: 1,
            ok: false,
            status: None,
            error: Some(ErrorReport {
                code: ErrorCode::Internal,
                message: "x".repeat(MAX_FRAME_BYTES),
            }),
        };
        assert!(matches!(
            write_response(&big),
            Err(WireError::FrameTooLarge)
        ));
    }
}
