//! Running a program for the editor, off the editor's thread.
//!
//! The kernel asks for a command and carries on with what it can do itself; this
//! is the half that has a process. Three things are worth knowing about its
//! shape.
//!
//! **The waiting is here.** The editor is one thread, and a build that takes a
//! minute would be a minute of a frozen screen with nobody listening for a
//! keystroke — so nothing about running a command may block the editor, and the
//! answer arrives at a tick.
//!
//! **Every pipe has a thread of its own.** Reading standard output to the end and
//! *then* reading standard error is a deadlock waiting for a compiler: the
//! program fills the second pipe while the first is still open, and neither side
//! moves again.
//!
//! **`:kill` sets a flag, and the thread that is waiting does the killing.** A
//! child behind a mutex its waiter holds for the whole wait is a child nothing
//! can kill.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, TryRecvError};

/// How often the thread that waits looks up from what it is doing.
const POLL: Duration = Duration::from_millis(20);

/// How long the pipes are given after the process is gone.
///
/// A pipe can outlive the process it was opened for — on Windows a killed shell
/// leaves its own child holding it — and waiting for a pipe forever would leave
/// the editor with a command that never finishes, which is worse than an answer
/// that is short by whatever had not arrived.
const GRACE: Duration = Duration::from_secs(2);

/// A command being run, and how to stop it.
pub(crate) struct Job {
    kill: Arc<AtomicBool>,
    result: Receiver<Outcome>,
}

/// What a finished command produced.
pub(crate) struct Outcome {
    /// Everything the command wrote to standard output.
    pub(crate) output: String,
    /// And to standard error, which is where a compiler puts the reason.
    pub(crate) stderr: String,
    /// The exit code, or `None` when the command was killed.
    pub(crate) status: Option<i32>,
}

impl Job {
    /// Ask the command to stop.
    ///
    /// The asking is a flag rather than a signal: the thread that owns the wait
    /// is the thread that sends it, since it is the only one that can without
    /// holding the child still while it does.
    pub(crate) fn kill(&self) {
        self.kill.store(true, Ordering::SeqCst);
    }

    /// What the command produced, if it is done.
    pub(crate) fn outcome(&self) -> Result<Outcome, TryRecvError> {
        self.result.try_recv()
    }
}

/// Start `command`, with `input` written to it.
///
/// The platform's shell runs it — `cmd /C` on Windows, `sh -c` elsewhere —
/// because what a person types on a `:` line is a command *line*: quoting, pipes
/// and redirection are the shell's business, and splitting it here would be a
/// second, worse shell.
pub(crate) fn spawn(
    command: &str,
    input: Option<&str>,
    cwd: &std::path::Path,
) -> std::io::Result<Job> {
    let mut child = shell(command, cwd)
        .stdin(match input {
            Some(_) => Stdio::piped(),
            None => Stdio::null(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let kill = Arc::new(AtomicBool::new(false));
    let asked = Arc::clone(&kill);
    let (tx, result) = bounded(1);

    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().expect("standard output was piped");
    let mut stderr = child.stderr.take().expect("standard error was piped");

    // The input goes in on a thread of its own. A program that reads nothing
    // until it has written something would otherwise hold a write up while a
    // read waits for it, and the two would wait for each other with both pipes
    // full.
    let writer = input.map(|text| {
        let text = text.to_string();
        let mut stdin = stdin.take();
        thread::Builder::new()
            .name("inkstone-job-stdin".to_string())
            .spawn(move || {
                if let Some(pipe) = stdin.as_mut() {
                    let _ = pipe.write_all(text.as_bytes());
                    let _ = pipe.flush();
                }
                // Dropping it closes the pipe, which is how the program learns
                // that that was all of the input there is.
                drop(stdin);
            })
    });

    let (out_tx, out_rx) = bounded(1);
    let (err_tx, err_rx) = bounded(1);
    thread::Builder::new()
        .name("inkstone-job-stdout".to_string())
        .spawn(move || {
            let _ = out_tx.send(read_to_string(&mut stdout));
        })?;
    thread::Builder::new()
        .name("inkstone-job-stderr".to_string())
        .spawn(move || {
            let _ = err_tx.send(read_to_string(&mut stderr));
        })?;

    thread::Builder::new()
        .name("inkstone-job-wait".to_string())
        .spawn(move || {
            let status = loop {
                if asked.load(Ordering::SeqCst) {
                    let _ = child.kill();
                }
                match child.try_wait() {
                    Ok(Some(status)) => break status.code(),
                    // Killed by a signal rather than an exit code.
                    Err(_) => break None,
                    Ok(None) => thread::sleep(POLL),
                }
            };
            let output = wait_for(&out_rx);
            let stderr = wait_for(&err_rx);
            if let Some(Ok(writer)) = writer {
                let _ = writer.join();
            }
            let _ = tx.send(Outcome {
                output,
                stderr,
                status,
            });
        })?;

    Ok(Job { kill, result })
}

/// The shell that runs a command line, with nothing of its own to show for it.
///
/// It runs in the session's directory: `:!cargo build` should build what a person
/// is looking at, not what the daemon happened to be started in.
fn shell(command: &str, cwd: &std::path::Path) -> Command {
    if cfg!(windows) {
        let mut shell = Command::new("cmd");
        shell.current_dir(cwd);
        shell.arg("/C").arg(command);
        // A console window of its own is not what a command from an editor
        // wants, and the windowed frontend has none to give it: without this
        // every `:!` would flash a window up.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            shell.creation_flags(CREATE_NO_WINDOW);
        }
        shell
    } else {
        let mut shell = Command::new("sh");
        shell.arg("-c").arg(command);
        shell.current_dir(cwd);
        shell
    }
}

/// What a pipe has to say, within the grace period.
fn wait_for(pipe: &Receiver<String>) -> String {
    pipe.recv_timeout(GRACE).unwrap_or_default()
}

/// Everything one pipe holds, as text.
///
/// Bytes that are not UTF-8 are replaced rather than refused: a command's output
/// is a terminal's output, and a terminal shows those bytes somehow.
fn read_to_string(pipe: &mut dyn Read) -> String {
    let mut bytes = Vec::new();
    let _ = pipe.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}
