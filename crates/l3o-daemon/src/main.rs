//! `l3od` — the L3O Audio engine daemon.
//!
//! M0 stub: it detects the platform backend and exits. The real-time engine and
//! the IPC control socket arrive in M1/M5.

use std::process::ExitCode;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let backend = l3o_platform::detect();
    tracing::info!(backend = backend.name(), "l3od starting");

    println!("l3od {} (M0 stub)", env!("CARGO_PKG_VERSION"));
    println!("platform backend: {}", backend.name());
    println!("engine: not wired yet (M1)");

    ExitCode::SUCCESS
}
