//! Running a program, and what its output is for.
//!
//! The kernel does not run anything: it asks, the way it asks a language server.
//! That is not indirection for its own sake. The editor is one thread, and a
//! build that takes ten seconds would be ten seconds of a frozen screen with
//! nobody listening for `<C-c>`; so the session runs the command on a thread of
//! its own and hands the answer back at a tick — see `docs/jobs.md`.
//!
//! What lives here is what makes a command worth running: the three things vim's
//! `!` can be. `:{range}!cmd` is a filter, and the range is the program's input.
//! `:r !cmd` reads its output into the text. `:!cmd` runs one on its own, and
//! what it said becomes a document — because output *is* text, and text in this
//! editor lives in a buffer.

use super::*;

/// A command the editor wants run, and what the program's input becomes.
#[derive(Debug, Clone)]
pub struct JobRequest {
    /// The command line, exactly as it was typed after the `!`.
    pub command: String,
    /// What standard input gets: the text of a filter's range, or nothing at
    /// all, which is an empty input rather than a terminal.
    pub input: Option<String>,
}

/// A command that is being run, and what its output is for.
#[derive(Debug, Clone)]
pub(crate) struct Job {
    /// The command line, for the messages that name it.
    command: String,
    /// Whether the session has taken it, since a job is asked for once.
    started: bool,
    /// What the answer will be done with.
    action: JobAction,
}

/// What the output of a running command is for.
#[derive(Debug, Clone)]
enum JobAction {
    /// `:!cmd` — a document of its own.
    Shell,
    /// `:r !cmd` — inserted below this line, if it still says the same thing.
    Read { row: usize, line: String },
    /// `:{range}!cmd` — replacing this text, if it is still that text.
    Filter {
        start: usize,
        end: usize,
        text: String,
    },
    /// `ed.shell(cmd, callback)` — what the command said goes to Lua.
    Call { callback: String },
}

impl Editor {
    /// `!{cmd}` — run a program, with `range`'s lines as its input when a range
    /// was named.
    pub(crate) fn run_shell(&mut self, command: &str, range: Option<(usize, usize)>) {
        let command = command.trim();
        if command.is_empty() {
            self.message = "E471: Argument required".to_string();
            return;
        }
        if let Some(running) = &self.job {
            self.message = format!("E: {} is still running (:kill stops it)", running.command);
            return;
        }
        // With a range the lines are the program's input and its output replaces
        // them: what makes a filter a filter is that it is one text becoming
        // another, and the range is what says which text.
        let action = match range {
            Some((from, to)) => {
                let last = self.buffer.line_count().saturating_sub(1);
                let (from, to) = (from.min(last), to.min(last));
                let (from, to) = (from.min(to), from.max(to));
                let start = self.buffer.line_col_to_char(from, 0);
                let end = if to < last {
                    self.buffer.line_col_to_char(to + 1, 0)
                } else {
                    self.buffer.len_chars()
                };
                JobAction::Filter {
                    start,
                    end,
                    text: self.buffer.slice_text(start, end),
                }
            }
            None => JobAction::Shell,
        };
        self.start_job(command, action);
    }

    /// `:r !cmd` — what a program says, below the line the cursor is on.
    pub(crate) fn read_command(&mut self, command: &str) {
        let command = command.trim();
        if command.is_empty() {
            self.message = "E471: Argument required".to_string();
            return;
        }
        if let Some(running) = &self.job {
            self.message = format!("E: {} is still running (:kill stops it)", running.command);
            return;
        }
        let row = self
            .cursor
            .row
            .min(self.buffer.line_count().saturating_sub(1));
        let line = self.buffer.line_string(row);
        self.start_job(command, JobAction::Read { row, line });
    }

    /// Run `command` for a script, and call `callback` with what it said.
    ///
    /// The script's half of `:!`: the same job, the same threads, and the same
    /// one-at-a-time rule — the difference is where the answer goes.
    pub(crate) fn shell_with_callback(&mut self, command: &str, callback: &str) {
        let command = command.trim();
        if command.is_empty() {
            self.message = "E471: Argument required".to_string();
            return;
        }
        if let Some(running) = &self.job {
            self.message = format!("E: {} is still running (:kill stops it)", running.command);
            return;
        }
        self.start_job(
            command,
            JobAction::Call {
                callback: callback.to_string(),
            },
        );
    }

    /// `:kill` — stop the command that is running.
    pub(crate) fn kill_job(&mut self) {
        if self.job.is_none() {
            self.message = "E: no command is running".to_string();
            return;
        }
        self.job_kill = true;
    }

    fn start_job(&mut self, command: &str, action: JobAction) {
        self.job = Some(Job {
            command: command.to_string(),
            started: false,
            action,
        });
        // Vim echoes the command it runs; the `!` is what says it is a command
        // line rather than a command.
        self.message = format!("!{command}");
        self.redraw = true;
    }

    /// The command the session should run, if one is waiting to be started.
    pub fn job_request(&mut self) -> Option<JobRequest> {
        let job = self.job.as_mut()?;
        if job.started {
            return None;
        }
        job.started = true;
        let input = match &job.action {
            JobAction::Filter { text, .. } => Some(text.clone()),
            JobAction::Shell | JobAction::Read { .. } | JobAction::Call { .. } => None,
        };
        Some(JobRequest {
            command: job.command.clone(),
            input,
        })
    }

    /// Whether `:kill` has been asked for, and forget that it was.
    pub fn take_job_kill(&mut self) -> bool {
        std::mem::take(&mut self.job_kill)
    }

    /// A command that could not be started at all.
    pub fn job_failed(&mut self, message: String) {
        let Some(job) = self.job.take() else {
            return;
        };
        self.message = format!("E: cannot run \"{}\": {message}", job.command);
        self.redraw = true;
    }

    /// What the command produced, at last.
    ///
    /// The answer is applied to the document that asked — and only if it is still
    /// the document that asked: a filter's range and a `:r !` line are held as
    /// the *text* they were, and text that has moved on is refused rather than
    /// replaced. Vim never has to ask, because it waits; a command that runs
    /// while a person goes on editing is the price of not freezing, and this is
    /// what it costs.
    pub fn job_finished(&mut self, output: String, stderr: String, status: Option<i32>) {
        let Some(job) = self.job.take() else {
            return;
        };
        // A command that failed says so whatever else it said: the code, and the
        // first line of what it wrote to its error stream, which is where the
        // reason is.
        let failed = status != Some(0);
        match job.action {
            JobAction::Shell => {
                // A command that said nothing opens nothing: what happened to it
                // is what the message row is for, and an empty window would be a
                // window nobody asked for.
                if !output.is_empty() || !stderr.is_empty() {
                    self.show_command_output(&job.command, &output, &stderr);
                }
                self.message = format!("!{}", job.command);
            }
            JobAction::Read { row, line } => {
                if self.buffer.line_string(row) != line {
                    self.message = stale(&job.command);
                    self.redraw = true;
                    return;
                }
                let at = self.buffer.line_col_to_char(row, self.buffer.line_len(row))
                    + self.buffer.line_terminator_len(row);
                let endings = self.buffer.line_terminator(row);
                let text = as_lines(&matching_endings(&output, &endings));
                let count = text.lines().count();
                self.replace_range(at, at, &text, Some(at));
                self.after_edit();
                self.message = format!(
                    "{count} line{} read from \"!{}\"",
                    if count == 1 { "" } else { "s" },
                    job.command
                );
            }
            JobAction::Filter { start, end, text } => {
                if self.buffer.slice_text(start, end) != text {
                    self.message = stale(&job.command);
                    self.redraw = true;
                    return;
                }
                let replacement = as_lines(&matching_endings(&output, &text));
                let count = replacement.lines().count();
                self.replace_range(start, end, &replacement, Some(start));
                self.after_edit();
                self.message = format!("{count} lines filtered");
            }
            JobAction::Call { callback } => {
                // The output and the code, in that order; a command that was
                // killed has no code, and `-1` says so rather than saying zero.
                let code = status.map(i64::from).unwrap_or(-1);
                if let Err(error) = self
                    .lua
                    .call_command(&callback, &[Param::Str(output), Param::Int(code)])
                {
                    self.message = format!("{callback}: {error}");
                    self.redraw = true;
                    return;
                }
                // Whatever the callback asked for runs now: a callback that
                // cannot act is a callback that cannot do anything.
                self.run_pending_lua();
                self.message = format!("!{}", job.command);
            }
        }
        if failed {
            self.message = shell_returned(status, &stderr);
        }
        self.redraw = true;
    }

    /// A command's output, as a document of its own in a window below.
    ///
    /// Both streams go in, in that order, when there are two of them. A terminal
    /// shows them interleaved because both are writing to it, and the errors of a
    /// build are on the second one — so a document holding only the first would
    /// hold the half nobody asked for.
    fn show_command_output(&mut self, command: &str, output: &str, stderr: &str) {
        let mut text = output.to_string();
        if !stderr.is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(stderr);
        }
        let mut buffer = Buffer::from_text(&as_lines(&text));
        buffer.set_title(&format!("!{command}"));
        let document = self.documents.len();
        self.documents.push(Document::new(buffer, Language::Plain));
        let editing = self.current_window;
        self.split_window(Axis::Rows, Some(document));
        // The window that asked keeps the cursor: the output is there to be read,
        // and the place somebody was editing is where they go on editing.
        self.switch_to_window(editing);
    }
}

/// A program's output as buffer lines.
///
/// A program that ends its output without a newline still ends its *result* with
/// one, because the line model needs it: a buffer line ends with its terminator,
/// and text that does not would be normalized into one anyway.
fn as_lines(output: &str) -> String {
    if output.is_empty() || output.ends_with('\n') {
        return output.to_string();
    }
    format!("{output}\n")
}

/// `text` written with the line endings `wanted` uses.
///
/// A program writes the endings its platform taught it — `sort` on Windows ends
/// every line with CRLF — and a filter is not allowed to change the file's. What
/// it replaces is the *text*; whether a line ends with LF or CRLF is the line
/// model's business, and a file written with CRLF that comes back from `:%!sort`
/// as LF has been rewritten behind somebody's back.
fn matching_endings(text: &str, wanted: &str) -> String {
    let terminator = if wanted.contains("\r\n") {
        "\r\n"
    } else if wanted.contains('\r') {
        "\r"
    } else {
        "\n"
    };
    let mut out = String::with_capacity(text.len());
    for piece in text.split_inclusive('\n') {
        let Some(line) = piece.strip_suffix('\n') else {
            out.push_str(piece);
            break;
        };
        out.push_str(line.strip_suffix('\r').unwrap_or(line));
        out.push_str(terminator);
    }
    out
}

/// What vim says when a command did not succeed.
fn shell_returned(status: Option<i32>, stderr: &str) -> String {
    let line = stderr.lines().next().unwrap_or("").trim();
    match (status, line.is_empty()) {
        (Some(code), true) => format!("shell returned {code}"),
        (Some(code), false) => format!("shell returned {code}: {line}"),
        (None, true) => "shell killed".to_string(),
        (None, false) => format!("shell killed: {line}"),
    }
}

/// The refusal for an answer that arrived after the text moved on.
fn stale(command: &str) -> String {
    format!("E: the text changed while \"{command}\" ran")
}
