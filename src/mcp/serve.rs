//! Starts and stops `rojo serve` processes for agents and talks to Rojo
//! servers through their agent API.

use std::{
    collections::{BTreeMap, VecDeque},
    env,
    fmt::Write as _,
    io::{BufRead, BufReader, Read},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use memofs::Vfs;
use reqwest::{blocking::Client, StatusCode};

use crate::{
    project::Project,
    web::{
        interface::{
            AgentConnectRequest, AgentConnectResponse, AgentStatusResponse, ErrorResponse,
        },
        util::{deserialize_msgpack, serialize_msgpack},
    },
};

/// The port `rojo serve` and the Studio plugin use when nothing else is set.
pub const DEFAULT_PORT: u16 = 34872;

/// Lines of server output kept for `serve_logs`.
const MAX_LOG_LINES: usize = 1000;

/// How long a server may take to load its project before `serve_start` gives
/// up. Large projects can take a while to snapshot.
const START_TIMEOUT: Duration = Duration::from_secs(120);

/// A command that runs this same Rojo executable, so child processes always
/// match the version of the MCP server.
pub fn rojo_command() -> Command {
    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("rojo"));
    let mut command = Command::new(exe);
    command.stdin(Stdio::null());
    command
}

struct ManagedServer {
    project: PathBuf,
    address: SocketAddr,
    child: Child,
    /// Never written to. The server stops once this is closed, which happens
    /// when it is dropped or when this process exits for any reason.
    _stdin: ChildStdin,
    log: Arc<Mutex<VecDeque<String>>>,
}

impl ManagedServer {
    fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn log_tail(&self, lines: usize) -> String {
        let log = self.log.lock().unwrap();
        let skip = log.len().saturating_sub(lines);
        log.iter()
            .skip(skip)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The Rojo servers started by this MCP server, by port.
pub struct ServeManager {
    servers: Mutex<BTreeMap<u16, ManagedServer>>,
    client: Client,
}

impl ServeManager {
    pub fn new() -> Self {
        Self {
            servers: Mutex::new(BTreeMap::new()),
            client: Client::builder()
                .timeout(Duration::from_secs(5))
                // Requests only ever go to a local Rojo server. A system proxy
                // would not be able to reach it.
                .no_proxy()
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    pub fn start(
        &self,
        project: &Path,
        port: Option<u16>,
        address: Option<IpAddr>,
    ) -> Result<String, String> {
        // Use the same defaults `rojo serve` would, so the port is known to
        // the agent and the plugin.
        let project_file = Project::load_fuzzy(&Vfs::new_default_without_watcher(), project)
            .map_err(|err| {
                format!(
                    "Could not load the project at {}: {}",
                    project.display(),
                    err
                )
            })?
            .ok_or_else(|| format!("No Rojo project found at {}", project.display()))?;

        let port = port.or(project_file.serve_port).unwrap_or(DEFAULT_PORT);
        let bind = address
            .or(project_file.serve_address)
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let address = SocketAddr::new(reachable_ip(bind), port);

        {
            let mut servers = self.servers.lock().unwrap();
            if let Some(server) = servers.get_mut(&port) {
                if server.is_running() {
                    return Err(format!(
                        "This MCP server is already serving {} on port {}. Stop it with \
                         serve_stop first, or pick another port.",
                        server.project.display(),
                        port
                    ));
                }
                if let Some(server) = servers.remove(&port) {
                    server.stop();
                }
            }
        }

        // Without this check, a server someone else started on the port could
        // answer the readiness check below while ours fails to bind.
        if TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_ok() {
            return Err(match self.status_at(address) {
                Ok(status) => format!(
                    "Port {} is already used by a Rojo server for project '{}' that this MCP \
                     server did not start. Use it as is (serve_status and studio_connect accept \
                     `port`), stop it, or pick another port.",
                    port, status.project_name
                ),
                Err(_) => format!(
                    "Port {} is already used by another program. Pick another port.",
                    port
                ),
            });
        }

        let mut command = rojo_command();
        command
            .arg("serve")
            .arg(project)
            .args(["--port", &port.to_string()])
            .args(["--address", &bind.to_string()])
            .args(["--exit-on-stdin-eof", "--color", "never"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .map_err(|err| format!("Could not start rojo serve: {}", err))?;

        let log = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stdout) = child.stdout.take() {
            collect_lines(stdout, Arc::clone(&log));
        }
        if let Some(stderr) = child.stderr.take() {
            collect_lines(stderr, Arc::clone(&log));
        }

        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            return Err("Could not open the standard input of rojo serve".to_owned());
        };

        let mut server = ManagedServer {
            project: project.to_path_buf(),
            address,
            child,
            _stdin: stdin,
            log,
        };

        let deadline = Instant::now() + START_TIMEOUT;
        let status = loop {
            if !server.is_running() {
                let output = server.log_tail(50);
                server.stop();
                return Err(format!("rojo serve stopped while starting:\n{}", output));
            }

            if let Ok(status) = self.status_at(address) {
                break status;
            }

            if Instant::now() > deadline {
                let output = server.log_tail(50);
                server.stop();
                return Err(format!(
                    "rojo serve did not start answering on {} in time:\n{}",
                    address, output
                ));
            }

            thread::sleep(Duration::from_millis(200));
        };

        let pid = server.child.id();
        self.servers.lock().unwrap().insert(port, server);

        Ok(format!(
            "Serving project '{}' from {} at {}:{} (pid {}).\n\
             Studio is not connected yet. Call studio_connect to have it connect, or connect \
             from the Rojo plugin in Studio.",
            status.project_name,
            project.display(),
            display_host(bind),
            port,
            pid
        ))
    }

    pub fn stop(&self, port: Option<u16>) -> Result<String, String> {
        let mut servers = self.servers.lock().unwrap();
        let port = pick_port(&servers, port)?;
        let server = servers
            .remove(&port)
            .ok_or_else(|| format!("This MCP server did not start a server on port {}", port))?;

        let project = server.project.clone();
        server.stop();

        Ok(format!(
            "Stopped serving {} on port {}",
            project.display(),
            port
        ))
    }

    pub fn stop_all(&self) {
        let servers = std::mem::take(&mut *self.servers.lock().unwrap());
        for (_, server) in servers {
            server.stop();
        }
    }

    pub fn status(&self, port: Option<u16>) -> Result<String, String> {
        let mut servers = self.servers.lock().unwrap();

        // A port this MCP server does not manage may still belong to a server
        // the user started by hand.
        if let Some(port) = port {
            if !servers.contains_key(&port) {
                let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
                return self.status_at(address).map(|status| {
                    format!(
                        "Port {} (not started by this MCP server):\n{}",
                        port,
                        describe_status(&status)
                    )
                });
            }
        }

        if servers.is_empty() {
            return Ok(format!(
                "This MCP server has not started any Rojo servers. Pass `port` to check a \
                 server started elsewhere, such as {}.",
                DEFAULT_PORT
            ));
        }

        let mut out = String::new();
        for (&server_port, server) in servers.iter_mut() {
            if port.is_some_and(|port| port != server_port) {
                continue;
            }

            let _ = writeln!(
                out,
                "Port {}: {} (pid {})",
                server_port,
                server.project.display(),
                server.child.id()
            );

            if !server.is_running() {
                let _ = writeln!(
                    out,
                    "  Stopped. Last output:\n{}",
                    indent(&server.log_tail(20))
                );
                continue;
            }

            match self.status_at(server.address) {
                Ok(status) => {
                    let _ = writeln!(out, "{}", describe_status(&status));
                }
                Err(err) => {
                    let _ = writeln!(out, "  Running, but not answering: {}", err);
                }
            }
        }

        Ok(out.trim_end().to_owned())
    }

    pub fn logs(&self, port: Option<u16>, lines: usize) -> Result<String, String> {
        let servers = self.servers.lock().unwrap();
        let port = pick_port(&servers, port)?;
        let server = servers
            .get(&port)
            .ok_or_else(|| format!("This MCP server did not start a server on port {}", port))?;

        let tail = server.log_tail(lines);
        Ok(if tail.is_empty() {
            "The server has not written any output yet.".to_owned()
        } else {
            tail
        })
    }

    /// Asks Studio to connect to the server on `port` and waits up to `wait`
    /// for a client to finish connecting.
    pub fn connect_studio(
        &self,
        port: Option<u16>,
        place_id: Option<u64>,
        wait: Duration,
    ) -> Result<String, String> {
        let address = {
            let servers = self.servers.lock().unwrap();
            match port {
                Some(port) => servers.get(&port).map(|server| server.address),
                None => match servers.len() {
                    0 => None,
                    1 => servers.values().next().map(|server| server.address),
                    _ => return Err(several_servers_error(&servers)),
                },
            }
            .unwrap_or_else(|| {
                SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    port.unwrap_or(DEFAULT_PORT),
                )
            })
        };

        let before = self.status_at(address)?;
        if before.connected_clients > 0 && place_id.is_none() {
            return Ok(format!(
                "Studio is already connected to project '{}' on port {} ({} client(s)).",
                before.project_name,
                address.port(),
                before.connected_clients
            ));
        }

        let request = self.request_connection(address, place_id)?;

        let deadline = Instant::now() + wait;
        loop {
            let status = self.status_at(address)?;

            // A different server took over the port, so the request is gone.
            if status.session_id != request.session_id {
                return Err(format!(
                    "The Rojo server on port {} restarted while waiting for Studio",
                    address.port()
                ));
            }

            if status.connected_clients > before.connected_clients {
                return Ok(format!(
                    "Studio connected to project '{}' on port {}. Rojo now syncs file changes \
                     into Studio.",
                    status.project_name,
                    address.port()
                ));
            }

            if Instant::now() >= deadline {
                let still_pending = status
                    .connect_request
                    .is_some_and(|pending| pending.id == request.request.id);

                return Ok(format!(
                    "Asked Studio to connect to port {}, but no client has connected yet{}.\n\
                     For Studio to connect on its own, it must be open with the Rojo plugin's \
                     \"Agent Connections\" setting on, and the plugin must be set to this host \
                     and port (localhost:{} is the default). If the plugin asks to confirm the \
                     first sync, someone has to accept it in Studio. The request stays open for \
                     2 minutes; check with serve_status.",
                    address.port(),
                    if still_pending {
                        ""
                    } else {
                        " and the request was answered or expired"
                    },
                    DEFAULT_PORT
                ));
            }

            thread::sleep(Duration::from_millis(250));
        }
    }

    fn status_at(&self, address: SocketAddr) -> Result<AgentStatusResponse, String> {
        let url = format!("http://{}/api/agent/status", address);
        let response = self
            .client
            .get(&url)
            .send()
            .map_err(|err| format!("No Rojo server is answering at {}: {}", address, err))?;

        read_response(response, address)
    }

    fn request_connection(
        &self,
        address: SocketAddr,
        place_id: Option<u64>,
    ) -> Result<AgentConnectResponse, String> {
        let body = serialize_msgpack(AgentConnectRequest { place_id })
            .map_err(|err| format!("Could not encode the connection request: {}", err))?;

        let response = self
            .client
            .post(format!("http://{}/api/agent/connect", address))
            .header(reqwest::header::CONTENT_TYPE, "application/msgpack")
            .body(body)
            .send()
            .map_err(|err| format!("No Rojo server is answering at {}: {}", address, err))?;

        read_response(response, address)
    }
}

impl Drop for ServeManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}

fn read_response<T: for<'de> serde::Deserialize<'de>>(
    response: reqwest::blocking::Response,
    address: SocketAddr,
) -> Result<T, String> {
    let status = response.status();
    let body = response
        .bytes()
        .map_err(|err| format!("Could not read the response from {}: {}", address, err))?;

    if status == StatusCode::NOT_FOUND {
        return Err(format!(
            "The Rojo server at {} does not support agents. It needs to be a Rojo version with \
             `rojo mcp`.",
            address
        ));
    }

    if !status.is_success() {
        let details = deserialize_msgpack::<ErrorResponse>(&body)
            .map(|error| format!("{:?}", error))
            .unwrap_or_else(|_| String::from_utf8_lossy(&body).into_owned());
        return Err(format!(
            "The Rojo server at {} answered {}: {}",
            address, status, details
        ));
    }

    deserialize_msgpack(&body).map_err(|err| {
        format!(
            "Unexpected response from the Rojo server at {}: {}",
            address, err
        )
    })
}

fn describe_status(status: &AgentStatusResponse) -> String {
    let mut out = format!(
        "  Project: {}\n  Rojo version: {}\n  Studio clients connected: {}",
        status.project_name, status.server_version, status.connected_clients
    );

    if let Some(request) = &status.connect_request {
        let _ = write!(
            out,
            "\n  Waiting for Studio to answer connection request #{}",
            request.id
        );
        if let Some(place_id) = request.place_id {
            let _ = write!(out, " (place {})", place_id);
        }
    }

    out
}

fn pick_port(servers: &BTreeMap<u16, ManagedServer>, port: Option<u16>) -> Result<u16, String> {
    match port {
        Some(port) => Ok(port),
        None => match servers.len() {
            0 => Err("This MCP server has not started any Rojo servers".to_owned()),
            1 => Ok(*servers.keys().next().unwrap()),
            _ => Err(several_servers_error(servers)),
        },
    }
}

fn several_servers_error(servers: &BTreeMap<u16, ManagedServer>) -> String {
    let ports: Vec<String> = servers.keys().map(u16::to_string).collect();
    format!(
        "Several servers are running (ports {}). Pass `port` to pick one.",
        ports.join(", ")
    )
}

/// The address to reach a server bound to `bind` from this machine.
fn reachable_ip(bind: IpAddr) -> IpAddr {
    match bind {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        ip => ip,
    }
}

fn display_host(bind: IpAddr) -> String {
    if bind.is_loopback() {
        "localhost".to_owned()
    } else {
        bind.to_string()
    }
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {}", line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Keeps the latest lines a child process writes, so they can be shown when
/// it fails or when an agent asks for them.
fn collect_lines(stream: impl Read + Send + 'static, log: Arc<Mutex<VecDeque<String>>>) {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            let mut log = log.lock().unwrap();
            if log.len() >= MAX_LOG_LINES {
                log.pop_front();
            }
            log.push_back(line);
        }
    });
}
