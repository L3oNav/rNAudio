//! `l3od` library: the daemon state machine, the versioned local IPC
//! protocol, and its Unix socket server. The binary in `main.rs` is a thin
//! shell over this so the CLI can share the protocol and tests can drive the
//! server in-process.

pub mod daemon;
pub mod ipc;
pub mod server;
