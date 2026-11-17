//! The port a project is served on when neither `--port` nor the project's
//! `servePort` sets one.
//!
//! A dev script can pick a port once and hand it to every command through
//! `ROJO_SERVE_PORT`: `rojo build` writes it into the place, so the plugin
//! knows where to connect as soon as Studio opens it, and `rojo serve`
//! listens on it, even when another tool such as rotor starts it.

use std::net::{IpAddr, TcpListener};

pub const DEFAULT_PORT: u16 = 34872;

/// How many ports from `DEFAULT_PORT` up `rojo serve` picks from. The plugin
/// scans the same range to find servers whose port it doesn't know, so keep
/// the two in step (`AutoConnect.SCAN_PORTS` in plugin/src/AutoConnect.lua).
pub const AUTO_PORT_COUNT: u16 = 28;

/// Environment variable naming the port to build for and serve on.
pub const PORT_ENV: &str = "ROJO_SERVE_PORT";

/// The port from `ROJO_SERVE_PORT`, if it is set to a valid nonzero port.
pub fn from_env() -> Option<u16> {
    let value = std::env::var(PORT_ENV).ok()?;
    match value.trim().parse::<u16>() {
        Ok(port) if port != 0 => Some(port),
        _ => {
            log::warn!("Ignoring {PORT_ENV}={value:?}, which is not a port number");
            None
        }
    }
}

/// The first port in the auto range that can be bound on `ip` right now.
/// Another process could take it before the server binds, in which case
/// starting fails as it would for a busy fixed port.
pub fn first_free_port(ip: IpAddr) -> Option<u16> {
    (DEFAULT_PORT..DEFAULT_PORT + AUTO_PORT_COUNT)
        .find(|&port| TcpListener::bind((ip, port)).map(drop).is_ok())
}
