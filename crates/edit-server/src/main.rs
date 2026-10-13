//! `edit-server` — the headless editor daemon.
//!
//! Reads [`edit_rpc::ClientMsg`] frames from stdin and writes
//! [`edit_rpc::ServerMsg`] frames to stdout. Normally spawned by the `edit`
//! frontend, but usable on its own for scripting and testing.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use edit_server::{run_stdio, InitConfig};

#[derive(Debug, Parser)]
#[command(
    name = "edit-server",
    version,
    about = "Headless editor daemon speaking msgpack RPC over stdio"
)]
struct Args {
    /// Source this Lua file instead of the default init.lua.
    #[arg(long, value_name = "FILE", conflicts_with = "no_init")]
    init: Option<PathBuf>,

    /// Do not source any Lua configuration.
    #[arg(long)]
    no_init: bool,
}

impl Args {
    fn init_config(&self) -> InitConfig {
        match (&self.init, self.no_init) {
            (Some(path), _) => InitConfig::Path(path.clone()),
            (None, true) => InitConfig::None,
            (None, false) => InitConfig::Default,
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run_stdio(std::io::stdin(), std::io::stdout(), args.init_config()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("edit-server: {e:#}");
            ExitCode::FAILURE
        }
    }
}
