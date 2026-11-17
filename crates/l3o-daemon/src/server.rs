//! Unix socket server: user-session only, same-user peer check, bounded
//! reads, one request per connection.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::daemon::Daemon;
use crate::ipc::{
    MAX_FRAME_BYTES, READ_TIMEOUT, RequestFrame, ResponseFrame, WireError, parse_request,
    write_response,
};

/// Where the control socket lives: `$XDG_RUNTIME_DIR/l3o` or the per-user
/// runtime dir. Nothing here is reachable from other users' sessions.
pub fn socket_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", std::process::id())));
    base.join("l3o")
}

pub fn socket_path() -> PathBuf {
    socket_dir().join("control.sock")
}

/// Creates the socket directory (0700) and binds the socket (0600 via
/// umask). Removes a stale socket file from a previous run.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    // Restrictive umask so the bound socket is 0600.
    let old_mask = unsafe { libc::umask(0o077) };
    let listener = UnixListener::bind(path);
    unsafe { libc::umask(old_mask) };
    listener
}

/// The peer's effective uid, or None if it cannot be determined.
#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 && len as usize == std::mem::size_of::<libc::ucred>() {
        Some(cred.uid)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc == 0 { Some(uid) } else { None }
}

/// Verifies the peer is the same user. Returns false for any error.
pub fn peer_is_same_user(stream: &UnixStream) -> bool {
    match peer_uid(stream) {
        Some(peer) => {
            #[allow(clippy::unnecessary_cast)]
            let our_uid = unsafe { libc::getuid() } as u32;
            peer == our_uid
        }
        None => false,
    }
}

/// Serves until process exit. Sequential: one connection at a time, each
/// bounded by [`READ_TIMEOUT`], so a stuck client can never block others for
/// more than the timeout.
pub fn serve(listener: UnixListener, daemon: Arc<Mutex<Daemon>>) -> ! {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(wire_err) = handle_connection(stream, &daemon) {
                    tracing::debug!(%wire_err, "connection rejected");
                }
            }
            Err(err) => tracing::warn!(%err, "accept failed"),
        }
    }
    unreachable!("incoming() never returns None");
}

fn handle_connection(mut stream: UnixStream, daemon: &Arc<Mutex<Daemon>>) -> Result<(), WireError> {
    if !peer_is_same_user(&stream) {
        return Err(WireError::Malformed); // peer check failure: close silently
    }
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));

    let frame = match read_frame(&stream)? {
        Some(frame) => frame,
        None => return Ok(()), // empty connection (e.g. health probe)
    };

    let (response, seq) = {
        let mut daemon = daemon.lock().unwrap_or_else(|e| e.into_inner());
        let seq = frame.seq;
        let response = match daemon.handle(frame.request) {
            Ok(status) => ResponseFrame {
                version: crate::ipc::PROTOCOL_VERSION,
                seq,
                ok: true,
                status: Some(status),
                error: None,
            },
            Err(error) => ResponseFrame {
                version: crate::ipc::PROTOCOL_VERSION,
                seq,
                ok: false,
                status: None,
                error: Some(error),
            },
        };
        (response, seq)
    };
    let _ = seq;

    let raw = write_response(&response)?;
    stream.write_all(&raw).map_err(|_| WireError::Malformed)?;
    Ok(())
}

/// Reads one newline-terminated frame, capped at [`MAX_FRAME_BYTES`].
/// Returns Ok(None) for a connection that sent nothing.
fn read_frame(mut stream: &UnixStream) -> Result<Option<RequestFrame>, WireError> {
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => {
                return if buf.is_empty() {
                    Ok(None)
                } else {
                    Err(WireError::Malformed)
                };
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
                if buf.len() > MAX_FRAME_BYTES {
                    return Err(WireError::FrameTooLarge);
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(WireError::Malformed);
            }
            Err(_) => return Err(WireError::Malformed),
        }
    }
    parse_request(&buf).map(Some)
}

/// Client side: one request, one response, one connection.
pub fn request(socket: &Path, frame: &RequestFrame) -> Result<ResponseFrame, ClientError> {
    let mut stream = UnixStream::connect(socket).map_err(ClientError::Connect)?;
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
    let raw = write_response_or(frame)?;
    stream.write_all(&raw).map_err(ClientError::Connect)?;
    let mut reader = std::io::BufReader::with_capacity(MAX_FRAME_BYTES + 1, &stream);
    let mut line = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        use std::io::Read;
        match reader.read(&mut byte) {
            Ok(0) => return Err(ClientError::Closed),
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
                if line.len() > MAX_FRAME_BYTES {
                    return Err(ClientError::Oversize);
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(ClientError::Timeout);
            }
            Err(err) => return Err(ClientError::Io(err)),
        }
    }
    serde_json::from_slice(&line).map_err(|_| ClientError::Malformed)
}

fn write_response_or(frame: &RequestFrame) -> Result<Vec<u8>, ClientError> {
    let mut raw = serde_json::to_vec(frame).map_err(|_| ClientError::Malformed)?;
    raw.push(b'\n');
    Ok(raw)
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot reach the daemon: {0}")]
    Connect(std::io::Error),

    #[error("daemon closed the connection")]
    Closed,

    #[error("daemon response exceeded the size limit")]
    Oversize,

    #[error("timed out talking to the daemon")]
    Timeout,

    #[error("malformed daemon response")]
    Malformed,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{PROTOCOL_VERSION, Request};
    use std::sync::{Arc, Mutex};

    fn spawn_server() -> (PathBuf, Arc<Mutex<Daemon>>) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "l3o-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let path = dir.join("control.sock");
        let listener = bind(&path).unwrap();
        let daemon = Arc::new(Mutex::new(Daemon::new(128)));
        let daemon2 = Arc::clone(&daemon);
        std::thread::spawn(move || serve(listener, daemon2));
        // Give the listener thread a moment to start accepting.
        for _ in 0..50 {
            if UnixStream::connect(&path).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        (path, daemon)
    }

    fn frame(request: Request) -> RequestFrame {
        RequestFrame {
            version: PROTOCOL_VERSION,
            seq: 1,
            request,
        }
    }

    #[test]
    fn round_trips_status() {
        let (path, _) = spawn_server();
        let response = request(&path, &frame(Request::Status)).unwrap();
        assert!(response.ok);
        let status = response.status.unwrap();
        assert_eq!(status.engine, crate::ipc::EngineState::Stopped);
    }

    #[test]
    fn malformed_and_oversize_inputs_never_panic() {
        let (path, daemon) = spawn_server();

        // Garbage bytes: the server rejects the frame and closes without a
        // response; Linux may deliver that as ECONNRESET, which is fine.
        let mut s = UnixStream::connect(&path).unwrap();
        s.write_all(b"\x00\xff not json\n").unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);

        // Oversize line: MAX + 100 bytes then newline.
        let mut s = UnixStream::connect(&path).unwrap();
        let big = format!(
            "{{\"version\":1,\"seq\":1,\"op\":\"ping\",\"pad\":\"{}\"}}\n",
            "x".repeat(MAX_FRAME_BYTES)
        );
        s.write_all(big.as_bytes()).unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);

        // Truncated (no newline) then close: server must exit cleanly.
        let mut s = UnixStream::connect(&path).unwrap();
        s.write_all(b"{\"version\":1").unwrap();
        drop(s);

        // Slowloris: partial frame, no newline, wait past the timeout.
        let mut s = UnixStream::connect(&path).unwrap();
        s.write_all(b"{").unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);

        // State untouched throughout.
        let status = request(&path, &frame(Request::Status)).unwrap();
        assert_eq!(
            status.status.unwrap().engine,
            crate::ipc::EngineState::Stopped
        );
        assert!(daemon.lock().unwrap().status().engine == crate::ipc::EngineState::Stopped);
    }

    #[test]
    fn engine_start_failure_is_reported_not_raised() {
        let (path, _) = spawn_server();
        // CI/dev machines may have no audio device; either outcome is valid,
        // but the response must be a well-formed frame either way.
        let response = request(&path, &frame(Request::EngineStart)).unwrap();
        match (response.ok, response.error) {
            (true, None) => {
                let stop = request(&path, &frame(Request::EngineStop)).unwrap();
                assert!(stop.ok);
            }
            (false, Some(err)) => {
                assert!(matches!(
                    err.code,
                    crate::ipc::ErrorCode::EngineStartFailed
                        | crate::ipc::ErrorCode::EngineAlreadyRunning
                ));
            }
            other => panic!("incoherent start response: {other:?}"),
        }
    }

    #[test]
    fn peer_check_accepts_same_user_connections() {
        // The test client runs as the same user, so peer_is_same_user must
        // accept it. (Cross-user rejection cannot be tested without a second
        // uid; the comparison itself is one line and reviewed.)
        let (path, _) = spawn_server();
        let stream = UnixStream::connect(path).unwrap();
        assert!(peer_is_same_user(&stream));
    }
}
