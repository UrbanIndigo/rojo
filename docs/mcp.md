# Using Rojo from AI agents (MCP)

`rojo mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io) server over standard input and output. Agents such as Claude Code, Cursor or VS Code can use it to work with a Rojo project on their own: start `rojo serve`, have Studio connect, build files, and find out which files make up which instances.

## Setup

1. Install this build of Rojo and its Studio plugin:

   ```sh
   cargo install --path .
   rojo plugin install
   ```

2. Register the server with your agent, pointing it at your project. With Claude Code:

   ```sh
   claude mcp add rojo -- rojo mcp path/to/project
   ```

   Clients configured through JSON use the same command:

   ```json
   {
     "mcpServers": {
       "rojo": { "command": "rojo", "args": ["mcp", "path/to/project"] }
     }
   }
   ```

   The project argument is optional and defaults to the folder the agent starts the server in. Tools also take a `project` argument to work on other projects.

3. In Studio, open the Rojo plugin's settings and turn on **Agent Connections**. Without it, the plugin ignores agents, and you connect by clicking Connect as usual.

## How Studio connects

The plugin cannot be reached from outside Studio, so it checks the server every two seconds while it is not connected. When an agent calls `studio_connect`, the server records a connection request, the plugin sees it and connects as if Connect had been clicked, and the agent is told once the initial sync went through.

- The plugin checks the host and port entered on its Connect page, and localhost ports 34872 to 34881. `serve_start` uses the project's `servePort`, or 34872, so servers on any of those ports are found without setting anything in Studio.
- Every plugin that is not connected sees a request, and the first one to claim it connects. With several Studio windows open, pass `place_id` or `place_name` to `studio_connect` so the right one answers. `place_name` is the name Studio shows for the place; a place opened from a file is named after the file, such as `lobby.rbxl`, which tells local files apart, since they all have place ID 0.
- **Confirmation Behavior** still applies. With the default, the plugin asks you to accept the first sync, and the agent waits for that. Set it to *Never* or *Unlisted PlaceId* (with `servePlaceIds` in the project) for agents to connect without anyone at the keyboard.
- A request is answered once, and expires after two minutes. Declining the sync does not lead to another attempt until the agent asks again.
- Only processes on the same machine can make requests, even when the server is bound to a network address.

## Tools

| Tool | What it does |
| --- | --- |
| `serve_start` | Starts `rojo serve` for a project in the background. The server stops with the MCP server, even if that is killed. |
| `serve_stop` | Stops a server started with `serve_start`. |
| `serve_status` | Shows each server's project, how many Studio clients are connected, and pending connection requests. With `port`, also works for a server started by hand. |
| `serve_logs` | Shows the latest server output, such as files that could not be synced. |
| `studio_connect` | Asks Studio to connect and waits for it, 30 seconds by default. |
| `build` | Builds a `.rbxl`, `.rbxlx`, `.rbxm` or `.rbxmx` file. |
| `sourcemap` | Writes a sourcemap for tools such as luau-lsp. |
| `find_instances` | Searches instances by name and class, with the files each comes from. |
| `get_instance` | Shows an instance's class, files and children. |
| `file_to_instance` | Shows which instances a file or folder turns into. |
| `instance_tree` | Shows the instance tree to a given depth. |

Instance paths look like `ReplicatedStorage.Shared.Util`, optionally starting with `game.`. Names that contain dots work too, and names can also be separated with `/`.

## Server API

Agents that talk to `rojo serve` directly can use the same two endpoints `rojo mcp` uses. Both use MessagePack, like the rest of the API.

- `GET /api/agent/status` returns `sessionId`, `serverVersion`, `projectName`, `connectedClients` (clients that finished their initial sync), and `connectRequest` (`{ id, placeId, placeName }` or nil).
- `POST /api/agent/connect` with an optional `{ placeId, placeName }` body records a connection request and returns it. Only local clients may call it.
- `POST /api/agent/claim` with `{ id }` hands the pending request to the caller and returns `{ claimed }`. Only the first claim of a request succeeds; plugins claim a request before they connect.
