//! A program that thinks it is talking to a terminal.
//!
//! `:term` runs a program on a *pseudo-terminal* rather than a pipe, which is
//! the difference between a program that prints and a program that draws: a
//! terminal is where it asks how wide it is, where it turns its own echo off,
//! and where the answers to "where is the cursor" come from. The process is the
//! session's — see `job.rs` — and the *screen* is the kernel's, because a screen
//! is what a buffer is: a grid of cells with a cursor in it.
//!
//! The kernel and the session pass three things between them, none of which is
//! on the wire: a program to start, bytes to write to it, and bytes it wrote.
//! The frontends are told nothing, and draw a terminal the way they draw
//! everything else, because it is a grid of cells like everything else.

use super::*;
use crate::screen::CHROME_ROWS;
use crate::terminal::Screen;

impl Editor {
    /// Send a key to the program, as the bytes a terminal would have sent.
    ///
    /// This is what "the keys are the program's" means: a character is its own
    /// UTF-8, a control key is the letter with its top bits taken off, and an
    /// arrow is the escape sequence every terminal has sent since the VT100.
    pub(crate) fn terminal_keys(&mut self, key: &KeyMsg) {
        let mut bytes: Vec<u8> = Vec::new();
        match key.code {
            KeyCode::Char(ch) if key.is_ctrl() => match ch.to_ascii_uppercase() {
                'A'..='Z' => bytes.push(ch.to_ascii_uppercase() as u8 - 0x40),
                '[' => bytes.push(0x1b),
                '\\' => bytes.push(0x1c),
                ']' => bytes.push(0x1d),
                _ => {}
            },
            KeyCode::Char(ch) => {
                let mut buffer = [0u8; 4];
                bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            }
            KeyCode::Enter => bytes.push(b'\r'),
            KeyCode::Esc => bytes.push(0x1b),
            KeyCode::Backspace => bytes.push(0x7f),
            KeyCode::Tab => bytes.push(b'\t'),
            KeyCode::Left => bytes.extend_from_slice(b"\x1b[D"),
            KeyCode::Right => bytes.extend_from_slice(b"\x1b[C"),
            KeyCode::Up => bytes.extend_from_slice(b"\x1b[A"),
            KeyCode::Down => bytes.extend_from_slice(b"\x1b[B"),
            KeyCode::Home => bytes.extend_from_slice(b"\x1b[H"),
            KeyCode::End => bytes.extend_from_slice(b"\x1b[F"),
            KeyCode::Delete => bytes.extend_from_slice(b"\x1b[3~"),
            KeyCode::PageUp => bytes.extend_from_slice(b"\x1b[5~"),
            KeyCode::PageDown => bytes.extend_from_slice(b"\x1b[6~"),
        }
        self.terminal_input.extend_from_slice(&bytes);
        self.redraw = true;
    }

    /// `:kill` for a terminal: ask the session to stop the program.
    pub(crate) fn kill_terminal(&mut self) {
        if self.terminal.is_some() {
            self.terminal_kill = true;
        }
    }

    /// `:term [program]` — a program that draws, in a window of its own.
    pub(crate) fn open_terminal(&mut self, program: Option<&str>) {
        if self.terminal.is_some() {
            self.message = "E: a terminal is already running (:kill stops it)".to_string();
            return;
        }
        let (cols, rows) = self.terminal_size();
        let program = program
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let name = match &program {
            Some(command) => command.clone(),
            None if cfg!(windows) => "cmd".to_string(),
            None => "sh".to_string(),
        };
        let screen = Screen::new(cols, rows);
        let document = self.documents.len();
        self.documents.push(Document::new(
            Buffer::from_terminal(&name, screen),
            Language::Plain,
        ));
        self.split_window(Axis::Rows, Some(document));
        // A terminal is for typing at, so the keys are the program's from the
        // first moment: the mode says so, and `<C-\>` is the way back out.
        self.mode = Mode::Insert;
        self.terminal = Some(Terminal {
            program,
            size: (cols, rows),
            started: false,
            resize: false,
        });
        self.message = format!("!{name}");
        self.redraw = true;
    }

    /// What the session needs to know: a program to start, if one is waiting.
    pub fn take_terminal_request(&mut self) -> Option<String> {
        let terminal = self.terminal.as_mut()?;
        if terminal.started {
            return None;
        }
        terminal.started = true;
        Some(terminal.program.clone().unwrap_or_default())
    }

    /// The size the program should be told about: the screen, less the two rows
    /// of chrome that belong to the editor.
    pub fn terminal_size(&self) -> (usize, usize) {
        let cols = self.viewport.width.max(1) as usize;
        let rows = (self.viewport.height.saturating_sub(CHROME_ROWS)).max(1) as usize;
        (cols, rows)
    }

    /// Whether a program is running in a terminal.
    pub(crate) fn terminal_running(&self) -> bool {
        self.terminal.is_some()
    }

    /// Whether a terminal is running, and what it was told its size was.
    pub fn terminal_wanted_size(&self) -> Option<(usize, usize)> {
        self.terminal.as_ref().map(|terminal| terminal.size)
    }

    /// Bytes the program should be written, if any are waiting.
    ///
    /// Keys and the answers to the program's own questions go the same way, and
    /// they are the same thing: input.
    pub fn take_terminal_input(&mut self) -> Option<Vec<u8>> {
        let mut bytes = std::mem::take(&mut self.terminal_input);
        // What the screen owes the program — the reply to "where is the cursor"
        // — is input too, and it is owed in the order it was asked for.
        if let Some(reply) = self.buffer.terminal_reply() {
            bytes.extend_from_slice(&reply);
        }
        if bytes.is_empty() {
            return None;
        }
        Some(bytes)
    }

    /// Whether `:kill` has been asked for the terminal, and forget that it was.
    pub fn take_terminal_kill(&mut self) -> bool {
        std::mem::take(&mut self.terminal_kill)
    }

    /// The program wrote.
    pub fn terminal_output(&mut self, bytes: &[u8]) {
        if self.terminal.is_none() {
            return;
        }
        self.buffer.terminal_feed(bytes);
        self.redraw = true;
    }

    /// The program is gone.
    ///
    /// Its screen stays — that is the scrollback, and the reason a terminal in a
    /// buffer is worth having — with a line saying what happened to it, and the
    /// keys go back to the editor: a dead program has nothing to type at.
    pub fn terminal_exited(&mut self, status: Option<i32>) {
        let Some(terminal) = self.terminal.take() else {
            return;
        };
        let _ = terminal;
        let said = match status {
            Some(code) => format!("\r\n[process exited {code}]\r\n"),
            None => "\r\n[process exited]\r\n".to_string(),
        };
        self.buffer.terminal_feed(said.as_bytes());
        if self.mode == Mode::Insert {
            self.mode = Mode::Normal;
        }
        self.message = said.trim().to_string();
        self.redraw = true;
    }

    /// Resize the terminal's screen, and remember the size to tell the program.
    pub(crate) fn resize_terminal(&mut self) {
        let (cols, rows) = self.terminal_size();
        let Some(terminal) = self.terminal.as_mut() else {
            return;
        };
        if terminal.size == (cols, rows) {
            return;
        }
        terminal.size = (cols, rows);
        terminal.resize = true;
    }

    /// The size the program has not been told yet, if it changed.
    pub fn take_terminal_resize(&mut self) -> Option<(usize, usize)> {
        let terminal = self.terminal.as_mut()?;
        if !terminal.resize {
            return None;
        }
        terminal.resize = false;
        Some(terminal.size)
    }
}

/// The terminal that is running, as the kernel remembers it.
#[derive(Debug)]
pub(crate) struct Terminal {
    /// The program that was asked for, or nothing for the platform's shell.
    program: Option<String>,
    /// The size the program has been told, in cells.
    size: (usize, usize),
    /// Whether the session has been asked to start it.
    started: bool,
    /// Whether the size has changed since it was last told.
    resize: bool,
}
