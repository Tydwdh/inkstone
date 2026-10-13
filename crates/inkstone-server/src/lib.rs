//! The `inkstone` daemon: owns the editor, speaks msgpack RPC over a byte stream.
//!
//! The daemon is deliberately thin. All the behaviour lives in [`inkstone_core`];
//! this crate is the seam that turns a stream of [`ClientMsg`]s into a stream
//! of [`ServerMsg`]s and nothing else. [`Session`] is that seam expressed as a
//! pure, blocking-free function so it can be driven from a test without
//! spawning a process or a thread.

use std::io::{BufWriter, Read, Write};
use std::path::PathBuf;
use std::thread;

use crossbeam_channel::{unbounded, Sender};

use anyhow::{Context, Result};
use inkstone_core::Editor;
use inkstone_rpc::{read_client_msg, write_server_msg, ClientMsg, ServerMsg};

/// How the daemon should find its Lua configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitConfig {
    /// Look for `init.lua` in the platform config directory.
    Default,
    /// Use this file.
    Path(PathBuf),
    /// Source nothing.
    None,
}

impl InitConfig {
    /// The file to source, if any.
    pub fn resolve(&self) -> Option<PathBuf> {
        match self {
            InitConfig::Default => Editor::default_init_path().filter(|p| p.exists()),
            InitConfig::Path(p) => Some(p.clone()),
            InitConfig::None => None,
        }
    }
}

/// The messages a handled client request produced.
#[derive(Debug)]
pub struct Response {
    /// Messages to send back to the frontend, in order.
    pub messages: Vec<ServerMsg>,
    /// Whether the session is over.
    pub quit: bool,
}

/// An editor plus the RPC session state around it.
pub struct Session {
    editor: Editor,
}

impl Session {
    pub fn new(editor: Editor) -> Self {
        Self { editor }
    }

    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// A redraw of the current state, for the very first frame.
    pub fn initial_frame(&mut self) -> ServerMsg {
        self.redraw()
    }

    /// Handle one client message.
    pub fn handle(&mut self, msg: ClientMsg) -> Response {
        match msg {
            ClientMsg::Shutdown => {
                return Response {
                    messages: Vec::new(),
                    quit: true,
                };
            }
            ClientMsg::Input(key) => self.editor.handle_key(key),
            ClientMsg::Resize { cols, rows } => self.editor.handle_resize(cols, rows),
            ClientMsg::Command { name, params } => self.editor.handle_command(&name, &params),
        }

        let mut messages = Vec::new();
        if self.editor.redraw {
            messages.push(self.redraw());
        }
        let quit = self.editor.should_quit;
        if quit {
            messages.push(ServerMsg::Quit);
        }
        Response { messages, quit }
    }

    fn redraw(&mut self) -> ServerMsg {
        self.editor.redraw = false;
        let composed = self.editor.compose();
        ServerMsg::Redraw {
            grid: composed.grid,
            cursor: composed.cursor,
            cursor_visible: composed.cursor_visible,
        }
    }
}

/// Run a session over a byte stream until the peer goes away or quits.
///
/// `input` is read on a helper thread; the main thread owns the editor and the
/// output, so the editor's state never crosses a thread boundary.
pub fn run_stdio(
    input: impl Read + Send + 'static,
    output: impl Write,
    init: InitConfig,
) -> Result<()> {
    let editor = Editor::new(init.resolve().as_deref())
        .map_err(|e| anyhow::anyhow!("failed to start the editor: {e}"))?;
    let mut session = Session::new(editor);
    let mut output = BufWriter::new(output);

    send(&mut output, &session.initial_frame())?;

    let (tx, rx) = unbounded::<ClientMsg>();
    thread::Builder::new()
        .name("inkstone-server-rpc-reader".to_string())
        .spawn(move || pump_input(input, tx))
        .context("failed to spawn the reader thread")?;

    while let Ok(msg) = rx.recv() {
        let response = session.handle(msg);
        for message in &response.messages {
            send(&mut output, message)?;
        }
        if response.quit {
            break;
        }
    }
    output.flush().context("failed to flush the output")?;
    Ok(())
}

fn send(output: &mut impl Write, msg: &ServerMsg) -> Result<()> {
    write_server_msg(output, msg).context("failed to write a frame")?;
    output.flush().context("failed to flush a frame")?;
    Ok(())
}

fn pump_input(mut input: impl Read, tx: Sender<ClientMsg>) {
    loop {
        match read_client_msg(&mut input) {
            Ok(Some(msg)) => {
                if tx.send(msg).is_err() {
                    return;
                }
            }
            Ok(None) => {
                // Clean end of stream: the frontend is gone.
                let _ = tx.send(ClientMsg::Shutdown);
                return;
            }
            Err(e) => {
                eprintln!("inkstone-server: rpc read error: {e}");
                let _ = tx.send(ClientMsg::Shutdown);
                return;
            }
        }
    }
}
