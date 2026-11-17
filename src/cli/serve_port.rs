//! Picks the port a project is served on when it doesn't set one, and
//! remembers it so `rojo build` and `rojo serve` agree.
//!
//! `rojo build` writes the port into the place it builds, so the plugin
//! knows where to connect as soon as Studio opens it. Whichever of the two
//! runs first picks a free port and records it; the other reads it.
//!
//! Ports are kept outside the project, in `~/.rojo/serve-ports`, keyed by
//! the project file's path. A file inside the project would need ignoring
//! in version control and could be seen by `rojo serve`'s file watcher.

use std::{
    fs,
    net::{IpAddr, TcpListener},
    path::{Path, PathBuf},
};

pub const DEFAULT_PORT: u16 = 34872;

/// How many ports from `DEFAULT_PORT` up are picked from. The plugin scans
/// the same range to find servers whose port it doesn't know, so keep the
/// two in step (`AutoConnect.SCAN_PORTS` in plugin/src/AutoConnect.lua).
pub const AUTO_PORT_COUNT: u16 = 28;

/// The first port in the auto range that can be bound on `ip` right now,
/// preferring ports no other project has recorded, so a project that is not
/// running keeps the port its built places name. Another process could take
/// the port before the server binds, in which case starting fails as it
/// would for a busy fixed port.
fn first_free_port(ip: IpAddr, project_file: &Path) -> Option<u16> {
    let taken = recorded_by_others(project_file);
    let range = DEFAULT_PORT..DEFAULT_PORT + AUTO_PORT_COUNT;
    range
        .clone()
        .find(|port| !taken.contains(port) && is_free(ip, *port))
        .or_else(|| range.clone().find(|&port| is_free(ip, port)))
}

fn recorded_by_others(project_file: &Path) -> Vec<u16> {
    let own = record_path(project_file);
    let Some(dir) = own.as_ref().and_then(|path| path.parent()) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| Some(entry.path()) != own)
        .filter_map(|entry| fs::read_to_string(entry.path()).ok())
        .filter_map(|contents| contents.trim().parse().ok())
        .collect()
}

pub fn is_free(ip: IpAddr, port: u16) -> bool {
    TcpListener::bind((ip, port)).map(drop).is_ok()
}

fn record_path(project_file: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let key: String = project_file
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(
        PathBuf::from(home)
            .join(".rojo")
            .join("serve-ports")
            .join(key),
    )
}

/// The port last picked for this project, if any.
pub fn remembered(project_file: &Path) -> Option<u16> {
    let contents = fs::read_to_string(record_path(project_file)?).ok()?;
    contents.trim().parse().ok().filter(|&port| port != 0)
}

/// Records the port picked for this project. Failing to record it only
/// means the next build or serve picks again, so errors are logged.
pub fn remember(project_file: &Path, port: u16) {
    let Some(path) = record_path(project_file) else {
        return;
    };
    let result = path
        .parent()
        .map(fs::create_dir_all)
        .unwrap_or(Ok(()))
        .and_then(|_| fs::write(&path, format!("{port}\n")));
    if let Err(err) = result {
        log::warn!("Could not remember serve port in {}: {err}", path.display());
    }
}

/// The port `rojo build` writes into a place for a project without a fixed
/// port: the remembered one, which a running `rojo serve` may hold, or else
/// a free one, recorded for `rojo serve` to use.
pub fn for_build(project_file: &Path, ip: IpAddr) -> Option<u16> {
    if let Some(port) = remembered(project_file) {
        return Some(port);
    }
    let port = first_free_port(ip, project_file)?;
    remember(project_file, port);
    Some(port)
}

/// The port `rojo serve` uses for a project without a fixed port: the
/// remembered one if it is free, or else the first free one, recorded so the
/// next build writes it into the place.
pub fn for_serve(project_file: &Path, ip: IpAddr) -> u16 {
    let remembered = remembered(project_file);
    if let Some(port) = remembered {
        if is_free(ip, port) {
            return port;
        }
    }

    let port = first_free_port(ip, project_file).unwrap_or(DEFAULT_PORT);
    if let Some(old) = remembered {
        log::warn!(
            "Port {old}, remembered for this project, is in use, so serving on {port}. \
             Places built before now name port {old}; the plugin still finds this \
             server, and rebuilding the place writes in the new port."
        );
    }
    remember(project_file, port);
    port
}
