//! Talking to the daemon: what every frontend needs, and none of them should
//! write twice.
//!
//! A frontend finds `inkstone-server`, spawns it, frames [`ClientMsg`]s at it,
//! and folds the [`ServerMsg`]s it sends back into a
//! [`Screen`](inkstone_rpc::Screen). That is all this crate does, so the
//! terminal frontend and the window frontend are the same program below the
//! drawing — which is the whole point of the split.

#![warn(missing_docs)]

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::thread;

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver};
use inkstone_rpc::{read_server_msg, write_client_msg, ClientMsg, ServerMsg};

/// A running daemon and the pipe to it.
///
/// The session ends when this is dropped or waited on: the pipe closes, and a
/// daemon that has nothing to read from exits.
pub struct Daemon {
    child: Child,
    stdin: ChildStdin,
    frames: Receiver<ServerMsg>,
}

impl Daemon {
    /// Start the daemon.
    ///
    /// `explicit` is the `--server` path a user gave; without it the daemon is
    /// looked for next to this executable, and then on `PATH`.
    pub fn spawn(explicit: Option<PathBuf>) -> Result<Self> {
        let program = resolve_server(explicit);
        let mut child = Command::new(&program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| startup_help(&program))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        Ok(Self {
            child,
            stdin,
            frames: spawn_reader(stdout),
        })
    }

    /// Send one message. An error means the daemon is gone.
    pub fn send(&mut self, msg: &ClientMsg) -> Result<()> {
        write_client_msg(&mut self.stdin, msg).context("failed to send a message to the daemon")?;
        self.stdin
            .flush()
            .context("failed to flush the daemon pipe")?;
        Ok(())
    }

    /// The frames the daemon has sent, in order.
    pub fn frames(&self) -> &Receiver<ServerMsg> {
        &self.frames
    }

    /// Close the pipe and wait for the daemon to exit.
    ///
    /// A frontend does this on the way out of a deliberate `Quit` and of an
    /// error path alike, so a child cannot outlive the session.
    pub fn wait(self) -> Result<ExitStatus> {
        let Self {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        child.wait().context("failed to wait for the daemon")
    }
}

/// The daemon binary: the explicit path, else a sibling of this executable,
/// else the bare name, for `PATH` to find.
pub fn resolve_server(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    let name = if cfg!(windows) {
        "inkstone-server.exe"
    } else {
        "inkstone-server"
    };
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)));
    match sibling {
        Some(path) if path.exists() => path,
        // Not built (or moved): fall back to whatever is on `PATH`.
        _ => PathBuf::from(name),
    }
}

/// What to tell a user whose daemon would not start.
fn startup_help(program: &Path) -> String {
    let looked_next_to = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.display().to_string()))
        .unwrap_or_else(|| "?".to_string());
    format!(
        "failed to start the daemon at {}\n\
         looked for it next to:   {looked_next_to}\n\
         build it first:          cargo build --workspace\n\
         or point at it:          --server <PATH>",
        program.display(),
    )
}

/// Decode daemon frames on a helper thread, so the frontend can select on them.
fn spawn_reader(stdout: std::process::ChildStdout) -> Receiver<ServerMsg> {
    let (tx, rx) = unbounded();
    thread::Builder::new()
        .name("inkstone-rpc-reader".to_string())
        .spawn(move || {
            let mut input = BufReader::new(stdout);
            while let Ok(Some(message)) = read_server_msg(&mut input) {
                if tx.send(message).is_err() {
                    return;
                }
            }
        })
        .expect("failed to spawn the reader thread");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_server_path_wins() {
        let path = PathBuf::from("somewhere/inkstone-server");
        assert_eq!(resolve_server(Some(path.clone())), path);
    }

    #[test]
    fn without_a_path_the_daemon_is_a_sibling_or_on_the_path() {
        let resolved = resolve_server(None);
        assert!(
            resolved.is_absolute() || resolved.components().count() == 1,
            "{resolved:?} is neither a sibling path nor a bare name"
        );
        let name = resolved.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("inkstone-server"), "{name}");
    }
}
