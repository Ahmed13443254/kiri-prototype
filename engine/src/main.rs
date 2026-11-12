// kiri-engine: clean-room agent engine.
// Contract: newline-delimited JSON messages and nested control envelopes.
// Speaks newline-delimited JSON over stdin/stdout. Never binds a socket.

mod agent;
mod api;
mod cli;
mod compact;
mod hooks;
mod memory;
mod out;
mod exec;
mod mcp;
mod prompt;
mod runtime;
mod permissions;
mod protocol;
mod session_state;
mod skills;
mod subagents;
mod tools;
mod workspaces;

use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    session_state::init_logger(cli.debug);
    let cfg = session_state::from_cli(cli)?;
    agent::run(cfg).await
}
