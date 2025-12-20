//! Daemon mode for Satty
//!
//! This module implements a daemon mode that keeps GTK4 initialized between
//! screenshot annotation calls, significantly reducing startup time.
//!
//! ## Architecture
//!
//! - **Daemon process**: Runs with `--daemon` flag, initializes GTK without showing a window,
//!   listens on a Unix socket for requests
//! - **Client process**: Runs with `--show` flag, connects to the daemon socket and sends
//!   image path + configuration, then exits
//! - **Multi-window**: Each request creates a new independent window
//!
//! ## Usage
//!
//! ```bash
//! # Start daemon (e.g., on login)
//! satty --daemon &
//!
//! # Send images to daemon
//! satty --show -f /tmp/screenshot.png -o /tmp/output.png
//! ```

pub mod protocol;
pub mod request_config;
pub mod security;
pub mod socket;

#[cfg(test)]
mod tests;

pub use protocol::{DaemonRequest, DaemonResponse, ResponseStatus};
pub use request_config::RequestConfig;
pub use security::validate_image_path;
pub use socket::{DaemonClient, DaemonServer};

use std::path::PathBuf;

/// Get the socket path for the current user
pub fn get_socket_path() -> PathBuf {
    let uid = nix::unistd::getuid();
    PathBuf::from(format!("/tmp/satty-{}.sock", uid))
}

/// Check if a daemon is already running
pub fn is_daemon_running() -> bool {
    let socket_path = get_socket_path();
    if !socket_path.exists() {
        return false;
    }

    // Try to connect to verify the daemon is actually running
    match std::os::unix::net::UnixStream::connect(&socket_path) {
        Ok(_) => true,
        Err(_) => {
            // Socket exists but can't connect - stale socket
            false
        }
    }
}

/// Remove stale socket file if it exists
pub fn remove_stale_socket() -> std::io::Result<()> {
    let socket_path = get_socket_path();
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }
    Ok(())
}
