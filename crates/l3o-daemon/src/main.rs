//! `l3od` — the L3O Audio engine daemon.
//!
//! Long-lived headless process: owns the mixer engine and exposes a
//! user-session Unix socket for local control. No network listener, no
//! elevated privileges, no client required.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use l3o_daemon::daemon::Daemon;
use l3o_daemon::server;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let path = server::socket_path();
    let listener = match server::bind(&path) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!(
                "error: cannot bind control socket {}: {err}",
                path.display()
            );
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(socket = %path.display(), "l3od listening");

    let daemon = Arc::new(Mutex::new(Daemon::new(128)));
    server::serve(listener, daemon)
}
