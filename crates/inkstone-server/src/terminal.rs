//! The pseudo-terminal behind `:term`.
//!
//! A program that draws needs a *terminal* on the other end, not a pipe: it asks
//! how wide it is, it turns its own echo off, and it expects the keys it gets to
//! be keys. That is what a pty is, and `portable-pty` is what opens one on both
//! platforms (ConPTY on Windows, `openpty` elsewhere).
//!
//! The shape is the job's shape, for the job's reason: nothing about a process
//! may block the editor. Reading and writing are threads with channels, and the
//! session's tick moves bytes between them and the kernel — which is where the
//! *screen* is, because a screen is what a buffer is.

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// How many bytes of a program's output are read at once.
const CHUNK: usize = 8 * 1024;

/// A program running on a pty of its own.
pub(crate) struct Terminal {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// What the program wrote, as it arrives.
    output: Receiver<Vec<u8>>,
    /// What to write to it: keys, and the answers to its own questions.
    input: Sender<Vec<u8>>,
    /// The size it was last told, so that a resize is sent once.
    size: (usize, usize),
}

impl Terminal {
    /// Start `program` — a command line, or nothing for the platform's shell —
    /// on a pty of this size.
    pub(crate) fn spawn(
        program: &str,
        cols: usize,
        rows: usize,
        cwd: &std::path::Path,
    ) -> Result<Self> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: rows.max(1) as u16,
                cols: cols.max(1) as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to open a pty")?;

        let command = shell_command(program, cwd);
        let child = pair
            .slave
            .spawn_command(command)
            .context("failed to start the program")?;
        // The slave end is the program's now; the master is all this side needs,
        // and dropping the slave is what lets the pty notice the program's exit.
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("failed to read from the pty")?;
        let mut writer = pair
            .master
            .take_writer()
            .context("failed to write to the pty")?;

        let (out_tx, output) = mpsc::channel();
        thread::Builder::new()
            .name("inkstone-terminal-read".to_string())
            .spawn(move || {
                let mut buffer = vec![0u8; CHUNK];
                loop {
                    match reader.read(&mut buffer) {
                        // The program is gone, or the pty is: either way there is
                        // nothing more to read.
                        Ok(0) | Err(_) => return,
                        Ok(read) => {
                            if out_tx.send(buffer[..read].to_vec()).is_err() {
                                return;
                            }
                        }
                    }
                }
            })
            .context("failed to spawn the terminal reader")?;

        let (input, in_rx) = mpsc::channel::<Vec<u8>>();
        thread::Builder::new()
            .name("inkstone-terminal-write".to_string())
            .spawn(move || {
                while let Ok(bytes) = in_rx.recv() {
                    if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                        return;
                    }
                }
            })
            .context("failed to spawn the terminal writer")?;

        Ok(Self {
            master: pair.master,
            child,
            output,
            input,
            size: (cols, rows),
        })
    }

    /// Everything the program has written since the last look.
    pub(crate) fn drain(&mut self) -> Vec<Vec<u8>> {
        let mut chunks = Vec::new();
        loop {
            match self.output.try_recv() {
                Ok(chunk) => chunks.push(chunk),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return chunks,
            }
        }
    }

    /// Write to the program.
    pub(crate) fn write(&self, bytes: Vec<u8>) {
        let _ = self.input.send(bytes);
    }

    /// Tell it how big it is now.
    pub(crate) fn resize(&mut self, cols: usize, rows: usize) {
        if self.size == (cols, rows) {
            return;
        }
        self.size = (cols, rows);
        let _ = self.master.resize(PtySize {
            rows: rows.max(1) as u16,
            cols: cols.max(1) as u16,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Stop it.
    pub(crate) fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Whether it has finished, and what it exited with.
    pub(crate) fn exited(&mut self) -> Option<Option<i32>> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.exit_code().try_into().ok()),
            // A pty that cannot be asked is a pty whose program is gone.
            Ok(None) => None,
            Err(_) => Some(None),
        }
    }
}

/// The command that runs a command line under a shell.
///
/// The same rule as `:!`: what a person types is a command *line*, and the shell
/// is what knows how to read one.
fn shell_command(program: &str, cwd: &std::path::Path) -> CommandBuilder {
    let program = program.trim();
    if program.is_empty() {
        let mut command = CommandBuilder::new_default_prog();
        command.cwd(cwd);
        return command;
    }
    if cfg!(windows) {
        let mut command = CommandBuilder::new("cmd");
        command.arg("/C");
        command.arg(program);
        command.cwd(cwd);
        command
    } else {
        let mut command = CommandBuilder::new("sh");
        command.arg("-c");
        command.arg(program);
        command.cwd(cwd);
        command
    }
}
