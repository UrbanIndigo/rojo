use std::path::PathBuf;

use clap::Parser;

use super::resolve_path;

/// Run a Model Context Protocol (MCP) server over standard input and output,
/// so AI agents can serve projects, connect Studio, and inspect instances on
/// their own.
///
/// Register it with your agent, for example:
/// `claude mcp add rojo -- rojo mcp path/to/project`
///
/// For Studio to connect when an agent asks, turn on "Agent Connections" in
/// the Rojo plugin's settings.
#[derive(Debug, Parser)]
pub struct McpCommand {
    /// The project tools use when the agent names none, and where relative
    /// paths start. Defaults to the current directory.
    #[clap(default_value = "")]
    pub project: PathBuf,
}

impl McpCommand {
    pub fn run(self) -> anyhow::Result<()> {
        let project = resolve_path(&self.project)?.into_owned();
        // A canonical path keeps paths in tool output consistent with the
        // ones `rojo sourcemap` reports.
        let project = dunce::canonicalize(&project).unwrap_or(project);

        crate::mcp::run_stdio(project)
    }
}
