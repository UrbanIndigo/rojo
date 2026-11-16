use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream},
    path::Path,
    process::{ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::{AtomicU16, Ordering},
    thread,
    time::{Duration, Instant},
};

use hyper_tungstenite::tungstenite::connect;
use librojo::web_api::AgentStatusResponse;
use serde_json::{json, Value};
use tempfile::{tempdir, TempDir};

use crate::rojo_test::{
    io_util::{KillOnDrop, ROJO_PATH},
    serve_util::deserialize_msgpack,
};

/// Ports for servers started through MCP, clear of the ones serve tests use.
fn next_port() -> u16 {
    static NEXT_PORT: AtomicU16 = AtomicU16::new(35700);
    NEXT_PORT.fetch_add(1, Ordering::SeqCst)
}

/// A place project with a script whose name contains a dot, to cover path
/// lookups for such names.
fn write_project() -> TempDir {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(
        root.join("default.project.json"),
        r#"{
            "name": "mcp_test",
            "tree": {
                "$className": "DataModel",
                "ReplicatedStorage": {
                    "Shared": { "$path": "src/shared" }
                },
                "ServerScriptService": { "$path": "src/server" }
            }
        }"#,
    )
    .unwrap();

    fs::create_dir_all(root.join("src/shared")).unwrap();
    fs::create_dir_all(root.join("src/server")).unwrap();
    fs::write(root.join("src/shared/Util.luau"), "return {}").unwrap();
    fs::write(root.join("src/shared/Util.spec.luau"), "return nil").unwrap();
    fs::write(root.join("src/server/main.server.luau"), "print('hi')").unwrap();

    dir
}

struct McpClient {
    process: KillOnDrop,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn start(project: &Path) -> Self {
        let mut child = Command::new(ROJO_PATH)
            .arg("mcp")
            .arg(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Couldn't start rojo mcp");

        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());

        let mut client = Self {
            process: KillOnDrop(child),
            stdin,
            stdout,
            next_id: 1,
        };

        let init = client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "rojo-tests", "version": "0" },
            }),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "rojo");
        client.notify("notifications/initialized");

        client
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", message).unwrap();
        stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method }));
    }

    /// Sends a request and returns its id without waiting for the response.
    fn send_request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    fn read_response(&mut self, id: u64) -> Value {
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).unwrap();
            assert!(read > 0, "rojo mcp closed its output");

            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|err| panic!("rojo mcp wrote invalid JSON ({}): {}", err, line));
            if message["id"] == id {
                return message;
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send_request(method, params);
        self.read_response(id)
    }

    fn tool_result(response: Value) -> (String, bool) {
        let result = &response["result"];
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("Unexpected tool response: {}", response))
            .to_owned();
        (text, result["isError"] == true)
    }

    /// Calls a tool and returns its text, failing the test on a tool error.
    fn call(&mut self, name: &str, arguments: Value) -> String {
        let (text, is_error) = self.try_call(name, arguments);
        assert!(!is_error, "{} failed: {}", name, text);
        text
    }

    fn try_call(&mut self, name: &str, arguments: Value) -> (String, bool) {
        Self::tool_result(self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        ))
    }
}

fn agent_status(port: u16) -> AgentStatusResponse {
    let body = reqwest::blocking::get(format!("http://localhost:{}/api/agent/status", port))
        .unwrap()
        .bytes()
        .unwrap();
    deserialize_msgpack(&body).unwrap()
}

fn is_listening(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "Timed out waiting until {}",
            what
        );
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn lists_tools() {
    let project = write_project();
    let mut client = McpClient::start(project.path());

    let response = client.request("tools/list", json!({}));
    let names: Vec<&str> = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();

    for expected in [
        "serve_start",
        "serve_stop",
        "serve_status",
        "serve_logs",
        "studio_connect",
        "build",
        "sourcemap",
        "find_instances",
        "get_instance",
        "file_to_instance",
        "instance_tree",
    ] {
        assert!(names.contains(&expected), "{} is not listed", expected);
    }

    assert_eq!(client.request("ping", json!({}))["result"], json!({}));
}

#[test]
fn looks_up_instances_and_files() {
    let project = write_project();
    let mut client = McpClient::start(project.path());

    let found = client.call("find_instances", json!({ "query": "util" }));
    assert!(
        found.contains("ReplicatedStorage.Shared.Util (ModuleScript) <- src/shared/Util.luau"),
        "{}",
        found
    );
    assert!(found.contains("ReplicatedStorage.Shared.Util.spec (ModuleScript)"));

    let scripts = client.call("find_instances", json!({ "class_name": "Script" }));
    assert!(
        scripts.contains("ServerScriptService.main (Script)"),
        "{}",
        scripts
    );

    let shared = client.call(
        "get_instance",
        json!({ "instance_path": "game.ReplicatedStorage.Shared" }),
    );
    assert!(shared.contains("Class: Folder"), "{}", shared);
    assert!(shared.contains("Files: src/shared"), "{}", shared);
    assert!(shared.contains("Util.spec (ModuleScript)"), "{}", shared);

    let spec = client.call(
        "get_instance",
        json!({ "instance_path": "ReplicatedStorage.Shared.Util.spec" }),
    );
    assert!(spec.contains("src/shared/Util.spec.luau"), "{}", spec);

    let (missing, is_error) = client.try_call(
        "get_instance",
        json!({ "instance_path": "ReplicatedStorage.Nope" }),
    );
    assert!(is_error);
    assert!(missing.contains("No instance at"), "{}", missing);

    let instance = client.call(
        "file_to_instance",
        json!({ "file_path": "src/server/main.server.luau" }),
    );
    assert_eq!(instance, "ServerScriptService.main (Script)");

    let tree = client.call("instance_tree", json!({ "depth": 1 }));
    assert!(tree.starts_with("mcp_test (DataModel)"), "{}", tree);
    assert!(
        tree.contains("  ReplicatedStorage (ReplicatedStorage)"),
        "{}",
        tree
    );
    assert!(!tree.contains("Util"), "{}", tree);
}

#[test]
fn builds_and_writes_sourcemaps() {
    let project = write_project();
    let mut client = McpClient::start(project.path());

    client.call("build", json!({ "output": "out/place.rbxl" }));
    assert!(project.path().join("out/place.rbxl").is_file());

    client.call("sourcemap", json!({ "output": "sourcemap.json" }));
    let sourcemap = fs::read_to_string(project.path().join("sourcemap.json")).unwrap();
    assert!(sourcemap.contains("Util.luau"));

    let (error, is_error) = client.try_call("build", json!({ "output": "out/place.txt" }));
    assert!(is_error, "{}", error);
}

#[test]
fn serves_and_reports_studio_connections() {
    let project = write_project();
    let mut client = McpClient::start(project.path());
    let port = next_port();

    let started = client.call("serve_start", json!({ "port": port }));
    assert!(
        started.contains("Serving project 'mcp_test'"),
        "{}",
        started
    );
    assert!(is_listening(port));

    let (again, is_error) = client.try_call("serve_start", json!({ "port": port }));
    assert!(is_error);
    assert!(again.contains("already serving"), "{}", again);

    let status = client.call("serve_status", json!({}));
    assert!(status.contains("Studio clients connected: 0"), "{}", status);

    // Nothing answers the request, so this times out right away.
    let waited = client.call("studio_connect", json!({ "wait_seconds": 0 }));
    assert!(waited.contains("no client has connected yet"), "{}", waited);
    let pending = agent_status(port)
        .connect_request
        .expect("no pending request");

    // Ask again, then act like a plugin with agent connections enabled: see
    // the new request and connect.
    let id = client.send_request(
        "tools/call",
        json!({ "name": "studio_connect", "arguments": { "wait_seconds": 30 } }),
    );
    wait_until("the new connection request is visible", || {
        agent_status(port)
            .connect_request
            .is_some_and(|request| request.id > pending.id)
    });

    let (mut socket, _) = connect(format!("ws://localhost:{}/api/socket/0", port)).unwrap();

    let (connected, is_error) = McpClient::tool_result(client.read_response(id));
    assert!(!is_error, "{}", connected);
    assert!(connected.contains("Studio connected"), "{}", connected);
    assert!(agent_status(port).connect_request.is_none());

    let already = client.call("studio_connect", json!({ "wait_seconds": 0 }));
    assert!(already.contains("already connected"), "{}", already);

    let status = client.call("serve_status", json!({ "port": port }));
    assert!(status.contains("Studio clients connected: 1"), "{}", status);

    let _ = socket.close(None);
    wait_until("the client disconnects", || {
        agent_status(port).connected_clients == 0
    });

    let logs = client.call("serve_logs", json!({}));
    assert!(logs.contains("agent requested"), "{}", logs);

    client.call("serve_stop", json!({}));
    assert!(!is_listening(port));
}

#[test]
fn servers_stop_when_the_mcp_server_is_killed() {
    let project = write_project();
    let mut client = McpClient::start(project.path());
    let port = next_port();

    client.call("serve_start", json!({ "port": port }));
    assert!(is_listening(port));

    // Killing the MCP server gives it no chance to clean up. The server it
    // started has to notice on its own.
    client.process.0.kill().unwrap();
    let _ = client.process.0.wait();
    drop(client.stdin.take());

    wait_until("the orphaned server stops", || !is_listening(port));
}

#[test]
fn reports_ports_used_by_other_servers() {
    let project = write_project();
    let port = next_port();

    let _other = KillOnDrop(
        Command::new(ROJO_PATH)
            .arg("serve")
            .arg(project.path())
            .args(["--port", &port.to_string()])
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_until("the other server is up", || is_listening(port));

    let mut client = McpClient::start(project.path());

    let (error, is_error) = client.try_call("serve_start", json!({ "port": port }));
    assert!(is_error);
    assert!(error.contains("did not start"), "{}", error);

    // Servers started elsewhere can still be inspected.
    let status = client.call("serve_status", json!({ "port": port }));
    assert!(
        status.contains("not started by this MCP server"),
        "{}",
        status
    );
    assert!(status.contains("Project: mcp_test"), "{}", status);
}
