//! The tools the MCP server offers, and how their arguments are read.

use std::{fs, net::IpAddr, path::PathBuf, time::Duration};

use serde_json::{json, Map, Value};

use super::{
    serve::{rojo_command, ServeManager},
    tree, RpcError,
};

/// Seconds `studio_connect` waits for Studio by default and at most. The
/// upper bound keeps a single tool call from blocking an agent for long.
const DEFAULT_CONNECT_WAIT: u64 = 30;
const MAX_CONNECT_WAIT: u64 = 120;

const DEFAULT_FIND_LIMIT: u64 = 50;
const MAX_FIND_LIMIT: u64 = 500;
const DEFAULT_TREE_DEPTH: u64 = 2;
const MAX_TREE_DEPTH: u64 = 10;
const MAX_LISTED_CHILDREN: usize = 200;

/// JSON schema for the `project` argument most tools take.
fn project_schema() -> Value {
    json!({
        "type": "string",
        "description": "Path to a project file or a folder with a default.project.json. \
                        Relative paths start from the project this MCP server was started \
                        for, which is also the default.",
    })
}

fn port_schema(description: &str) -> Value {
    json!({ "type": "integer", "minimum": 1, "maximum": 65535, "description": description })
}

pub fn definitions() -> Value {
    json!([
        {
            "name": "serve_start",
            "title": "Start serving a project",
            "description": "Start `rojo serve` for a project in the background so Studio can \
                            sync it. Use studio_connect afterwards to have Studio connect. The \
                            server stops when this MCP server stops.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project_schema(),
                    "port": port_schema("Port to serve on. Defaults to the project's servePort, \
                                         or 34872, which the Studio plugin uses by default."),
                    "address": {
                        "type": "string",
                        "description": "IP address to listen on. Defaults to the project's \
                                        serveAddress, or 127.0.0.1.",
                    },
                },
            },
        },
        {
            "name": "serve_stop",
            "title": "Stop serving",
            "description": "Stop a server started with serve_start. Studio stays as it is, but \
                            stops receiving changes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": port_schema("Port of the server. Optional when only one is running."),
                },
            },
        },
        {
            "name": "serve_status",
            "title": "Server status",
            "description": "Show the servers started with serve_start: project, whether Studio \
                            is connected, and pending connection requests. With `port`, also \
                            works for a server started outside this MCP server.",
            "annotations": { "readOnlyHint": true },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": port_schema("Only show the server on this port."),
                },
            },
        },
        {
            "name": "serve_logs",
            "title": "Server output",
            "description": "Show the latest output of a server started with serve_start, such as \
                            sync errors for files that could not be turned into instances.",
            "annotations": { "readOnlyHint": true },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": port_schema("Port of the server. Optional when only one is running."),
                    "lines": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 1000,
                        "description": "Number of lines to show. Defaults to 100.",
                    },
                },
            },
        },
        {
            "name": "studio_connect",
            "title": "Connect Studio",
            "description": "Ask Roblox Studio to connect to a Rojo server and wait for it. This \
                            works when Studio is open with the Rojo plugin's \"Agent \
                            Connections\" setting on and the plugin is set to the server's host \
                            and port. Returns once Studio connected or the wait ran out.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": port_schema("Port of the server. Defaults to the only server \
                                         started with serve_start, or 34872."),
                    "place_id": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Only a Studio window with this place open should \
                                        connect. Useful when several places are open.",
                    },
                    "wait_seconds": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_CONNECT_WAIT,
                        "description": "How long to wait for Studio. Defaults to 30.",
                    },
                },
            },
        },
        {
            "name": "build",
            "title": "Build a place or model file",
            "description": "Build a project into a place or model file (.rbxl, .rbxlx, .rbxm or \
                            .rbxmx), like `rojo build`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project_schema(),
                    "output": {
                        "type": "string",
                        "description": "File to write. Its extension picks the format. Relative \
                                        paths start from the default project's folder.",
                    },
                },
                "required": ["output"],
            },
        },
        {
            "name": "sourcemap",
            "title": "Write a sourcemap",
            "description": "Write a sourcemap file, which tools such as luau-lsp use to resolve \
                            requires, like `rojo sourcemap`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project_schema(),
                    "output": {
                        "type": "string",
                        "description": "File to write, usually sourcemap.json. Relative paths \
                                        start from the default project's folder.",
                    },
                    "include_non_scripts": {
                        "type": "boolean",
                        "description": "Include instances that are not scripts. Defaults to \
                                        false.",
                    },
                },
                "required": ["output"],
            },
        },
        {
            "name": "find_instances",
            "title": "Find instances",
            "description": "Search the project's instances by name and class, and show which \
                            files each one comes from. At least one of `query` and \
                            `class_name` is needed.",
            "annotations": { "readOnlyHint": true },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project_schema(),
                    "query": {
                        "type": "string",
                        "description": "Case-insensitive text the instance name contains. If \
                                        it contains a dot, it is matched against the full path \
                                        instead, like Shared.Util.",
                    },
                    "class_name": {
                        "type": "string",
                        "description": "Only instances of this class, like ModuleScript.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_FIND_LIMIT,
                        "description": "Most results to show. Defaults to 50.",
                    },
                },
            },
        },
        {
            "name": "get_instance",
            "title": "Get an instance",
            "description": "Show an instance's class, the files it comes from, and its \
                            children.",
            "annotations": { "readOnlyHint": true },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project_schema(),
                    "instance_path": {
                        "type": "string",
                        "description": "Path of the instance, like \
                                        ReplicatedStorage.Shared.Util. An empty path is the \
                                        project's root.",
                    },
                },
                "required": ["instance_path"],
            },
        },
        {
            "name": "file_to_instance",
            "title": "Find the instance for a file",
            "description": "Show which instances a file or folder on disk turns into, if it is \
                            part of the project.",
            "annotations": { "readOnlyHint": true },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project_schema(),
                    "file_path": {
                        "type": "string",
                        "description": "The file or folder. Relative paths start from the \
                                        default project's folder.",
                    },
                },
                "required": ["file_path"],
            },
        },
        {
            "name": "instance_tree",
            "title": "Show the instance tree",
            "description": "Show the project's instances as an indented tree, with the files \
                            they come from.",
            "annotations": { "readOnlyHint": true },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project_schema(),
                    "instance_path": {
                        "type": "string",
                        "description": "Start from this instance instead of the root.",
                    },
                    "depth": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_TREE_DEPTH,
                        "description": "Levels of children to show. Defaults to 2.",
                    },
                },
            },
        },
    ])
}

pub struct Tools {
    /// Where relative paths start and which project tools use by default.
    default_project: PathBuf,
    serve: ServeManager,
}

impl Tools {
    pub fn new(default_project: PathBuf) -> Self {
        Self {
            default_project,
            serve: ServeManager::new(),
        }
    }

    pub fn shutdown(&self) {
        self.serve.stop_all();
    }

    pub fn call(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("Missing tool name"))?;

        let empty = Map::new();
        let args = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(args)) => args,
            Some(_) => return Err(RpcError::invalid_params("Tool arguments must be an object")),
        };
        let args = Args(args);

        let result = match name {
            "serve_start" => self.serve_start(&args),
            "serve_stop" => args.port("port").and_then(|port| self.serve.stop(port)),
            "serve_status" => args.port("port").and_then(|port| self.serve.status(port)),
            "serve_logs" => self.serve_logs(&args),
            "studio_connect" => self.studio_connect(&args),
            "build" => self.build(&args),
            "sourcemap" => self.sourcemap(&args),
            "find_instances" => self.find_instances(&args),
            "get_instance" => self.get_instance(&args),
            "file_to_instance" => self.file_to_instance(&args),
            "instance_tree" => self.instance_tree(&args),
            _ => return Err(RpcError::invalid_params(format!("Unknown tool: {}", name))),
        };

        let (text, is_error) = match result {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        };

        Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "isError": is_error,
        }))
    }

    /// Resolves a path argument against the default project's folder.
    fn path(&self, path: &str) -> PathBuf {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return path;
        }

        // The default project may be a project file rather than a folder.
        let base = if self.default_project.is_file() {
            self.default_project
                .parent()
                .map(PathBuf::from)
                .unwrap_or_default()
        } else {
            self.default_project.clone()
        };
        base.join(path)
    }

    fn project(&self, args: &Args) -> Result<PathBuf, String> {
        let project = match args.string("project")? {
            Some(project) => self.path(project),
            None => self.default_project.clone(),
        };

        if !project.exists() {
            return Err(format!("No project found at {}", project.display()));
        }

        Ok(project)
    }

    /// Resolves the `output` argument, creating its folder so agents can
    /// write into folders that do not exist yet, such as a build directory.
    fn output_path(&self, args: &Args) -> Result<PathBuf, String> {
        let output = self.path(args.required_string("output")?);

        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("Could not create {}: {}", parent.display(), err))?;
        }

        Ok(output)
    }

    /// The folder paths in output are shown relative to.
    fn project_dir(project: &std::path::Path) -> PathBuf {
        let dir = if project.is_file() {
            project.parent().map(PathBuf::from).unwrap_or_default()
        } else {
            project.to_path_buf()
        };
        dunce::canonicalize(&dir).unwrap_or(dir)
    }

    fn serve_start(&self, args: &Args) -> Result<String, String> {
        let project = self.project(args)?;
        let port = args.port("port")?;
        let address = args
            .string("address")?
            .map(|address| {
                address
                    .parse::<IpAddr>()
                    .map_err(|_| format!("'{}' is not an IP address", address))
            })
            .transpose()?;

        self.serve.start(&project, port, address)
    }

    fn serve_logs(&self, args: &Args) -> Result<String, String> {
        let lines = args.integer("lines", 1, 1000)?.unwrap_or(100);
        self.serve.logs(args.port("port")?, lines as usize)
    }

    fn studio_connect(&self, args: &Args) -> Result<String, String> {
        let port = args.port("port")?;
        let place_id = args.integer("place_id", 0, u64::MAX)?;
        let wait = args
            .integer("wait_seconds", 0, MAX_CONNECT_WAIT)?
            .unwrap_or(DEFAULT_CONNECT_WAIT);

        self.serve
            .connect_studio(port, place_id, Duration::from_secs(wait))
    }

    fn build(&self, args: &Args) -> Result<String, String> {
        let project = self.project(args)?;
        let output = self.output_path(args)?;

        run_rojo(
            rojo_command()
                .arg("build")
                .arg(&project)
                .arg("--output")
                .arg(&output),
        )?;

        Ok(format!(
            "Built {} into {}",
            project.display(),
            output.display()
        ))
    }

    fn sourcemap(&self, args: &Args) -> Result<String, String> {
        let project = self.project(args)?;
        let output = self.output_path(args)?;

        let mut command = rojo_command();
        command
            .arg("sourcemap")
            .arg(&project)
            .arg("--output")
            .arg(&output);
        if args.boolean("include_non_scripts")?.unwrap_or(false) {
            command.arg("--include-non-scripts");
        }
        run_rojo(&mut command)?;

        Ok(format!("Wrote the sourcemap to {}", output.display()))
    }

    fn find_instances(&self, args: &Args) -> Result<String, String> {
        let project = self.project(args)?;
        let query = args.string("query")?.filter(|query| !query.is_empty());
        let class_name = args.string("class_name")?.filter(|name| !name.is_empty());
        if query.is_none() && class_name.is_none() {
            return Err("Pass `query`, `class_name`, or both".to_owned());
        }
        let limit = args
            .integer("limit", 1, MAX_FIND_LIMIT)?
            .unwrap_or(DEFAULT_FIND_LIMIT) as usize;

        let root = tree::load(&project)?;
        let project_dir = Self::project_dir(&project);
        let (found, total) = tree::find(&root, query, class_name, limit);

        if found.is_empty() {
            return Ok("No instances match.".to_owned());
        }

        let mut out = found
            .iter()
            .map(|chain| {
                tree::describe(
                    chain[chain.len() - 1],
                    &tree::full_name(chain),
                    &project_dir,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        if total > found.len() {
            out.push_str(&format!(
                "\n... {} more. Narrow the search or raise `limit`.",
                total - found.len()
            ));
        }

        Ok(out)
    }

    fn get_instance(&self, args: &Args) -> Result<String, String> {
        let project = self.project(args)?;
        let path = args.required_string("instance_path")?;

        let root = tree::load(&project)?;
        let project_dir = Self::project_dir(&project);
        let chain = tree::resolve(&root, path).ok_or_else(|| not_found(path))?;
        let node = chain.last().copied().unwrap_or(&root);

        let mut out = format!(
            "Path: {}\nClass: {}\n",
            if chain.is_empty() {
                format!("(root, project '{}')", root.name)
            } else {
                tree::full_name(&chain)
            },
            node.class_name
        );

        if node.file_paths.is_empty() {
            out.push_str("Files: none, this instance is defined in the project file\n");
        } else {
            out.push_str(&format!(
                "Files: {}\n",
                tree::display_paths(&node.file_paths, &project_dir)
            ));
        }

        if node.children.is_empty() {
            out.push_str("Children: none");
        } else {
            out.push_str(&format!("Children ({}):\n", node.children.len()));
            out.push_str(&tree::list_children(node, MAX_LISTED_CHILDREN));
        }

        Ok(out.trim_end().to_owned())
    }

    fn file_to_instance(&self, args: &Args) -> Result<String, String> {
        let project = self.project(args)?;
        let file = self.path(args.required_string("file_path")?);
        let file = dunce::canonicalize(&file)
            .map_err(|err| format!("Could not find {}: {}", file.display(), err))?;

        let root = tree::load(&project)?;
        let found = tree::find_by_file(&root, &file);

        if found.is_empty() {
            return Ok(format!(
                "{} is not part of the project, or is ignored by it.",
                file.display()
            ));
        }

        Ok(found
            .iter()
            .map(|chain| {
                format!(
                    "{} ({})",
                    tree::full_name(chain),
                    chain[chain.len() - 1].class_name
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn instance_tree(&self, args: &Args) -> Result<String, String> {
        let project = self.project(args)?;
        let path = args.string("instance_path")?.unwrap_or("");
        let depth = args
            .integer("depth", 0, MAX_TREE_DEPTH)?
            .unwrap_or(DEFAULT_TREE_DEPTH) as usize;

        let root = tree::load(&project)?;
        let project_dir = Self::project_dir(&project);
        let chain = tree::resolve(&root, path).ok_or_else(|| not_found(path))?;

        Ok(match chain.last() {
            Some(node) => tree::outline(node, &tree::full_name(&chain), depth, &project_dir),
            None => tree::outline(&root, &root.name, depth, &project_dir),
        }
        .trim_end()
        .to_owned())
    }
}

fn not_found(path: &str) -> String {
    format!(
        "No instance at '{}'. Use find_instances or instance_tree to look for it.",
        path
    )
}

fn run_rojo(command: &mut std::process::Command) -> Result<(), String> {
    let output = command
        .args(["--color", "never"])
        .output()
        .map_err(|err| format!("Could not run Rojo: {}", err))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .trim()
        .to_owned())
    }
}

/// Typed access to tool arguments, with errors an agent can act on.
struct Args<'a>(&'a Map<String, Value>);

impl Args<'_> {
    fn string(&self, name: &str) -> Result<Option<&str>, String> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok(Some(value)),
            Some(_) => Err(format!("`{}` must be a string", name)),
        }
    }

    fn required_string(&self, name: &str) -> Result<&str, String> {
        self.string(name)?
            .ok_or_else(|| format!("`{}` is required", name))
    }

    fn boolean(&self, name: &str) -> Result<Option<bool>, String> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err(format!("`{}` must be true or false", name)),
        }
    }

    fn integer(&self, name: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
        let value = match self.0.get(name) {
            None | Some(Value::Null) => return Ok(None),
            Some(value) => value,
        };

        // Some clients send whole numbers as floats or strings.
        let number = value
            .as_u64()
            .or_else(|| {
                value
                    .as_f64()
                    .filter(|n| n.fract() == 0.0 && *n >= 0.0)
                    .map(|n| n as u64)
            })
            .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()));

        match number {
            Some(number) if (min..=max).contains(&number) => Ok(Some(number)),
            _ => Err(format!(
                "`{}` must be a whole number from {} to {}",
                name, min, max
            )),
        }
    }

    fn port(&self, name: &str) -> Result<Option<u16>, String> {
        Ok(self
            .integer(name, 1, u16::MAX as u64)?
            .map(|port| port as u16))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn args(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    #[test]
    fn integer_arguments_accept_whole_floats_and_strings_within_range() {
        let map = args(json!({ "a": 5, "b": 6.0, "c": "7", "d": 1.5, "e": 99999, "f": "x" }));
        let args = Args(&map);

        assert_eq!(args.integer("a", 0, 10), Ok(Some(5)));
        assert_eq!(args.integer("b", 0, 10), Ok(Some(6)));
        assert_eq!(args.integer("c", 0, 10), Ok(Some(7)));
        assert_eq!(args.integer("missing", 0, 10), Ok(None));
        assert!(args.integer("d", 0, 10).is_err());
        assert!(args.port("e").is_err());
        assert!(args.integer("f", 0, 10).is_err());
    }

    #[test]
    fn tool_failures_are_results_not_protocol_errors() {
        let tools = Tools::new(PathBuf::from("this-project-does-not-exist"));
        let result = tools
            .call(&json!({ "name": "find_instances", "arguments": { "query": "x" } }))
            .unwrap();

        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("No project found"));
    }

    #[test]
    fn bad_argument_types_are_reported() {
        let tools = Tools::new(PathBuf::from("."));
        let result = tools
            .call(&json!({ "name": "serve_logs", "arguments": { "port": "not a port" } }))
            .unwrap();

        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("`port`"));
    }
}
