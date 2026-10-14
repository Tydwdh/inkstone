//! `inkstone-gui` — the windowed frontend.
//!
//! The window frontend is deliberately as dumb as the terminal one: it forwards
//! keys, text and resizes to the daemon, and draws the grid the daemon sends.
//! It holds no document, no cursor and no modes — its only state is the
//! [`Screen`] every frame is folded into, which is a cache of the *screen* and
//! nothing else. Everything below the drawing — finding and starting the
//! daemon, framing, folding frames — is [`inkstone_client`].
//!
//! The one piece of plumbing a terminal does not need is the way frames reach
//! the event loop: the client reads them on its own thread, the event loop has
//! to be woken for each batch, and a heap of winit events is not a queue that
//! can be drained — so the bridge thread rings the doorbell
//! ([`EventLoopProxy::send_event`]) and the loop takes everything waiting on the
//! channel in one go. A burst of frames costs one repaint.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Result};
use clap::Parser;
use inkstone_client::Daemon;
use inkstone_rpc::{
    ClickCounter, ClientMsg, KeyCode, KeyMsg, Mod, Param, PointerKind, PointerMsg, Screen,
    ServerMsg,
};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize, Position, Size};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

mod glyphs;
mod render;
mod theme;

use render::Renderer;

/// The window a session opens with, in logical pixels, when nothing says
/// otherwise.
const INITIAL_WINDOW: (f64, f64) = (1024.0, 768.0);

#[derive(Debug, Parser)]
#[command(
    name = "inkstone-gui",
    version,
    about = "A modal editor: the same screen as the terminal frontend, in a window"
)]
struct Args {
    /// File to open. Created on write if it does not exist.
    file: Option<PathBuf>,

    /// The daemon to talk to. Defaults to `inkstone-server` next to this executable.
    #[arg(long, value_name = "PATH")]
    server: Option<PathBuf>,
}

/// What the bridge thread tells the event loop.
///
/// A winit event is a doorbell rather than a queue — one proxy event per frame
/// would be a redraw per frame, and the loop cannot ask "were there more?" — so
/// the frames themselves travel on a channel and this only says that it is worth
/// draining.
#[derive(Debug)]
enum UserEvent {
    /// At least one frame is waiting on the frame channel.
    Frame,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("inkstone-gui: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Start the daemon and the window, and run until the session ends.
fn run() -> Result<ExitCode> {
    let args = Args::parse();
    let daemon = Daemon::spawn(args.server)?;

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|err| anyhow!("the window system could not be started: {err}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let (frames_tx, frames_rx) = channel();
    spawn_bridge(
        daemon.frames().clone().into_iter(),
        frames_tx,
        event_loop.create_proxy(),
    )?;

    let mut app = App::new(args.file, daemon, frames_rx);
    event_loop
        .run_app(&mut app)
        .map_err(|err| anyhow!("the event loop stopped with an error: {err}"))?;

    // Whatever ended the session — `:q!`, the window closing, an error — the
    // daemon goes with it, so a window cannot leave an `inkstone-server` behind.
    app.finish();
    Ok(app.exit())
}

/// Forward the daemon's frames to the window's event loop.
///
/// The client already reads the daemon on a thread of its own and leaves the
/// frames in a channel; this thread turns "a frame arrived" into an event the
/// event loop will wake up for. `frames` is that channel's blocking iterator —
/// any iterator of frames will do, which is what keeps this frontend from having
/// to name the channel type `inkstone-client` hands it.
fn spawn_bridge<F>(frames: F, tx: Sender<ServerMsg>, proxy: EventLoopProxy<UserEvent>) -> Result<()>
where
    F: Iterator<Item = ServerMsg> + Send + 'static,
{
    thread::Builder::new()
        .name("inkstone-gui-bridge".to_string())
        .spawn(move || {
            for message in frames {
                // Either the app is gone (its end of the channel closed) or the
                // window is (the proxy refused the event); both mean stop.
                if tx.send(message).is_err() || proxy.send_event(UserEvent::Frame).is_err() {
                    return;
                }
            }
        })
        .map_err(|err| anyhow!("failed to start the frame bridge thread: {err}"))?;
    Ok(())
}

/// The window, the screen the daemon has drawn, and the pipe back to it.
struct App {
    /// Taken when the session ends, so the daemon is waited for exactly once.
    daemon: Option<Daemon>,
    /// The frames the bridge thread has forwarded, in order.
    frames: Receiver<ServerMsg>,
    file: Option<PathBuf>,
    /// The screen as the daemon has drawn it: this frontend's only state.
    screen: Screen,
    renderer: Option<Renderer>,
    window: Option<Arc<Window>>,
    /// The modifiers that were down for the last key press: winit reports them
    /// on their own events, not on the key itself.
    modifiers: ModifiersState,
    /// Where the pointer last was, in window pixels: a press, a drag and a
    /// release all report it separately, and the daemon wants a cell.
    position: PhysicalPosition<f64>,
    /// Whether the left button is down, which is what makes a move a drag.
    dragging: bool,
    /// How many clicks in a row the last press was: the frontend is the one with
    /// a clock, so it counts them, and the manual says what each count selects.
    clicks: ClickCounter,
    /// Whether the input method is currently allowed in, so the window is only
    /// told when the answer changes.
    ime_allowed: Option<bool>,
    /// Where the cursor was and whether it was shown, so a frame that only moves
    /// it can redraw the row it left as well as the row it moved to.
    cursor: Option<((u16, u16), bool)>,
    /// What to return from `main` when the loop stops.
    exit: ExitCode,
}

impl App {
    /// A frontend with nothing drawn yet.
    fn new(file: Option<PathBuf>, daemon: Daemon, frames: Receiver<ServerMsg>) -> Self {
        Self {
            daemon: Some(daemon),
            frames,
            file,
            screen: Screen::new(),
            renderer: None,
            window: None,
            modifiers: ModifiersState::empty(),
            position: PhysicalPosition::new(0.0, 0.0),
            dragging: false,
            clicks: ClickCounter::default(),
            ime_allowed: None,
            cursor: None,
            exit: ExitCode::SUCCESS,
        }
    }

    /// The exit code of the session.
    fn exit(&self) -> ExitCode {
        self.exit
    }

    /// Create the window, the GPU renderer, and open the file the user asked
    /// for.
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let attributes = Window::default_attributes()
            .with_title(title(self.file.as_deref()))
            .with_inner_size(LogicalSize::new(INITIAL_WINDOW.0, INITIAL_WINDOW.1));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|err| anyhow!("the window could not be created: {err}"))?,
        );
        // Off until a frame says otherwise: the editor starts in normal mode,
        // where an input method would swallow the keys that run commands. It is
        // allowed in again the moment a frame reports insert mode, which is how
        // anything outside ASCII gets typed at all.
        window.set_ime_allowed(false);

        let renderer = Renderer::new(window.clone(), window.scale_factor() as f32)?;
        let (cols, rows) = grid_size(window.inner_size(), renderer.cell_size());
        self.renderer = Some(renderer);
        self.window = Some(window);

        // The daemon needs the size before it composes anything: it lays the
        // screen out for exactly this many cells.
        self.send(&ClientMsg::Resize { cols, rows })?;
        if let Some(path) = self.file.clone() {
            self.send(&ClientMsg::Command {
                name: "edit_file".to_string(),
                params: vec![Param::Str(path.to_string_lossy().into_owned())],
            })?;
        }
        Ok(())
    }

    /// Send one message to the daemon.
    fn send(&mut self, message: &ClientMsg) -> Result<()> {
        match self.daemon.as_mut() {
            Some(daemon) => daemon.send(message),
            // The session is already over, so there is nothing left to say.
            None => Ok(()),
        }
    }

    /// Fold every frame that has arrived into the screen, and draw what changed.
    fn drain(&mut self, event_loop: &ActiveEventLoop) {
        let mut rows: Vec<u16> = Vec::new();
        loop {
            match self.frames.try_recv() {
                Ok(ServerMsg::Redraw {
                    grid,
                    cursor,
                    cursor_visible,
                    insert,
                }) => rows.extend(self.screen.redraw(grid, cursor, cursor_visible, insert)),
                Ok(ServerMsg::Rows {
                    updates,
                    cursor,
                    cursor_visible,
                    insert,
                }) => rows.extend(self.screen.rows(&updates, cursor, cursor_visible, insert)),
                Ok(ServerMsg::Clipboard(text)) => {
                    // The clipboard belongs to the system, and the window is the
                    // only one here that can reach it.
                    match arboard::Clipboard::new() {
                        Ok(mut clipboard) => {
                            if let Err(err) = clipboard.set_text(text) {
                                eprintln!("inkstone-gui: could not copy: {err}");
                            }
                        }
                        Err(err) => eprintln!("inkstone-gui: no clipboard: {err}"),
                    }
                }
                Ok(ServerMsg::Quit) => {
                    // The daemon is finished with this session (`:q!`), so the
                    // window goes too.
                    self.quit(event_loop);
                    return;
                }
                Err(TryRecvError::Empty) => break,
                // The daemon hung up: nothing left to draw, so leave cleanly.
                Err(TryRecvError::Disconnected) => {
                    self.quit(event_loop);
                    return;
                }
            }
        }
        self.redraw(rows);
    }

    /// Rebuild `rows` in the renderer and ask for a repaint.
    ///
    /// The cursor is drawn as part of its row, so a frame that moves it without
    /// changing any text still has to redraw both the row it left and the row it
    /// arrived at.
    fn redraw(&mut self, mut rows: Vec<u16>) {
        let cursor = (self.screen.cursor(), self.screen.cursor_visible());
        if self.cursor != Some(cursor) {
            if let Some((previous, _)) = self.cursor {
                rows.push(previous.1);
            }
            rows.push(cursor.0 .1);
            self.cursor = Some(cursor);
        }

        let height = self.screen.grid().height;
        rows.retain(|row| *row < height);
        rows.sort_unstable();
        rows.dedup();

        // An input method belongs in insert mode and nowhere else: left on in
        // normal mode, it eats exactly the keys that run commands (`dd`, `:wq`).
        if self.ime_allowed != Some(self.screen.insert()) {
            if let Some(window) = self.window.as_ref() {
                window.set_ime_allowed(self.screen.insert());
            }
            self.ime_allowed = Some(self.screen.insert());
        }

        if let Some(renderer) = self.renderer.as_mut() {
            renderer.update(
                self.screen.grid(),
                &rows,
                cursor.0,
                cursor.1,
                self.screen.insert(),
            );
        }
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Draw one frame, keeping the input method's candidate window with the
    /// cursor.
    fn paint(&mut self, event_loop: &ActiveEventLoop) {
        let cell = match self.renderer.as_ref() {
            Some(renderer) => renderer.cell_size(),
            None => return,
        };
        let cursor = self.screen.cursor();
        if let Some(window) = self.window.as_ref() {
            // Where the text being typed goes, so an input method puts its
            // candidate list next to the cursor rather than under the mouse.
            window.set_ime_cursor_area(
                Position::Physical(PhysicalPosition::new(
                    (f32::from(cursor.0) * cell.0) as i32,
                    (f32::from(cursor.1) * cell.1) as i32,
                )),
                Size::Physical(PhysicalSize::new(cell.0 as u32, cell.1 as u32)),
            );
        }
        if let Some(renderer) = self.renderer.as_mut() {
            if let Err(err) = renderer.render() {
                self.fail(event_loop, err);
            }
        }
    }

    /// End the session: tell the daemon, wait for it, and stop the loop.
    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        let _ = self.send(&ClientMsg::Shutdown);
        self.finish();
        event_loop.exit();
    }

    /// Wait for the daemon so it cannot outlive the window.
    fn finish(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.wait();
        }
    }

    /// Stop with a sentence a user can act on.
    fn fail(&mut self, event_loop: &ActiveEventLoop, err: anyhow::Error) {
        eprintln!("inkstone-gui: {err:#}");
        self.exit = ExitCode::FAILURE;
        self.quit(event_loop);
    }

    /// Where the pointer is, in screen cells, from a window position in pixels.
    fn cell_at(&self, position: PhysicalPosition<f64>) -> Option<(u16, u16)> {
        let cell = self.renderer.as_ref()?.cell_size();
        cell_at((position.x, position.y), cell)
    }

    /// Tell the daemon what the pointer did, in cells.
    fn pointer(
        &mut self,
        event_loop: &ActiveEventLoop,
        kind: PointerKind,
        position: PhysicalPosition<f64>,
    ) {
        let Some((x, y)) = self.cell_at(position) else {
            return;
        };
        // A press is counted where the clock is — here — so the daemon is told
        // how many clicks in a row it is: the same cell again quickly is a
        // double click, and the manual says what that selects.
        let msg = if kind == PointerKind::Press {
            let count = self.clicks.press(x, y, std::time::Instant::now());
            PointerMsg::click(kind, x, y, count)
        } else {
            PointerMsg::new(kind, x, y)
        };
        if let Err(err) = self.send(&ClientMsg::Pointer(msg)) {
            self.fail(event_loop, err);
        }
    }

    /// A window resize changes how many cells fit, which is the daemon's
    /// business as much as the surface's.
    fn resized(&mut self, event_loop: &ActiveEventLoop, size: PhysicalSize<u32>) {
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.resize(size.width, size.height);
        }
        // A minimised window has no cells at all; telling the daemon about a
        // 1x1 screen would throw the layout away and not give it back.
        if size.width == 0 || size.height == 0 {
            return;
        }
        let cell = match self.renderer.as_ref() {
            Some(renderer) => renderer.cell_size(),
            None => return,
        };
        let (cols, rows) = grid_size(size, cell);
        if let Err(err) = self.send(&ClientMsg::Resize { cols, rows }) {
            self.fail(event_loop, err);
            return;
        }
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// A key press becomes an input message, unless the protocol cannot spell
    /// the key — in which case it is dropped, exactly as the terminal frontend
    /// drops it.
    fn key_pressed(&mut self, event_loop: &ActiveEventLoop, event: &KeyEvent) {
        // Copy and paste are the window's own shortcuts, not the editor's keys:
        // a window that does not copy with Ctrl+C is broken, and the editor has
        // nothing to interrupt with it.
        if self.modifiers.control_key() && !self.modifiers.alt_key() {
            match shortcut(&event.logical_key) {
                Some(Shortcut::Copy) => {
                    if let Err(err) = self.send(&ClientMsg::Copy) {
                        self.fail(event_loop, err);
                    }
                    return;
                }
                Some(Shortcut::Paste) => {
                    self.paste(event_loop);
                    return;
                }
                None => {}
            }
        }
        let Some(message) = to_key_msg(&event.logical_key, self.modifiers) else {
            return;
        };
        if let Err(err) = self.send(&ClientMsg::Input(message)) {
            self.fail(event_loop, err);
        }
    }

    /// Paste what the system clipboard holds, as text the editor did not type.
    fn paste(&mut self, event_loop: &ActiveEventLoop) {
        let text = match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
            Ok(text) => text,
            // An empty clipboard, or one holding something that is not text, is
            // not an error: there is simply nothing to paste.
            Err(_) => return,
        };
        if let Err(err) = self.send(&ClientMsg::Text(text)) {
            self.fail(event_loop, err);
        }
    }

    /// Rebuild everything after the window's scale factor changed, since a cell
    /// — and so every glyph in the atlas — is sized in physical pixels.
    fn rescaled(&mut self, event_loop: &ActiveEventLoop, scale_factor: f64) {
        let Some(window) = self.window.clone() else {
            return;
        };
        // The old surface has to be gone before another is made for the same
        // window.
        self.renderer = None;
        match Renderer::new(window, scale_factor as f32) {
            Ok(renderer) => {
                self.renderer = Some(renderer);
                // The screen itself did not change, so the daemon will not send
                // it again: every row is rebuilt from the screen we hold.
                let rows = (0..self.screen.grid().height).collect();
                self.redraw(rows);
            }
            Err(err) => self.fail(event_loop, err),
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // winit resumes once per window, and this app has exactly one.
        if self.window.is_some() {
            return;
        }
        if let Err(err) = self.start(event_loop) {
            self.fail(event_loop, err);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Frame => self.drain(event_loop),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => self.quit(event_loop),
            WindowEvent::Resized(size) => self.resized(event_loop, size),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.rescaled(event_loop, scale_factor)
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.position = position;
                // Moving with the button down is a drag; moving without it is
                // nothing the editor needs to hear about.
                if self.dragging {
                    self.pointer(event_loop, PointerKind::Drag, position);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let position = self.position;
                match state {
                    ElementState::Pressed => {
                        self.dragging = true;
                        self.pointer(event_loop, PointerKind::Press, position);
                    }
                    ElementState::Released => {
                        self.dragging = false;
                        self.pointer(event_loop, PointerKind::Release, position);
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let cell = self
                    .renderer
                    .as_ref()
                    .map(|renderer| renderer.cell_size())
                    .unwrap_or((1.0, 1.0));
                let kind = match delta {
                    MouseScrollDelta::LineDelta(_, lines) if lines > 0.0 => PointerKind::ScrollUp,
                    MouseScrollDelta::LineDelta(_, lines) if lines < 0.0 => PointerKind::ScrollDown,
                    // A trackpad reports pixels; a scroll needs a cell's worth
                    // of them, or one flick would be a hundred messages.
                    MouseScrollDelta::PixelDelta(delta) if delta.y >= f64::from(cell.1) => {
                        PointerKind::ScrollUp
                    }
                    MouseScrollDelta::PixelDelta(delta) if delta.y <= -f64::from(cell.1) => {
                        PointerKind::ScrollDown
                    }
                    _ => return,
                };
                let position = self.position;
                self.pointer(event_loop, kind, position);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // The window reports a press and a release for every key; only
                // the press is a key the editor has not seen yet.
                if event.state != ElementState::Released {
                    self.key_pressed(event_loop, &event);
                }
            }
            // What an input method committed: text that was never a key, so it
            // is inserted rather than interpreted (a `:` typed through an input
            // method must not start a command).
            WindowEvent::Ime(Ime::Commit(text)) => {
                if let Err(err) = self.send(&ClientMsg::Text(text)) {
                    self.fail(event_loop, err);
                }
            }
            WindowEvent::RedrawRequested => self.paint(event_loop),
            _ => {}
        }
    }
}

/// Which screen cell a window position in pixels is in.
///
/// A cell is the unit the daemon lays out in, so this is the only place a
/// pointer position is translated — and it is the same arithmetic the window's
/// size is turned into a grid size with, which is what keeps a click on a cell
/// and a click on its neighbour one cell apart.
fn cell_at(position: (f64, f64), cell: (f32, f32)) -> Option<(u16, u16)> {
    if cell.0 <= 0.0 || cell.1 <= 0.0 {
        return None;
    }
    let x = position.0 / f64::from(cell.0);
    let y = position.1 / f64::from(cell.1);
    if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 {
        return None;
    }
    Some((x as u16, y as u16))
}

/// A window shortcut the editor does not have a key for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shortcut {
    /// `Ctrl+C`: put the selection, or the register, on the clipboard.
    Copy,
    /// `Ctrl+V`: insert what the clipboard holds.
    Paste,
}

/// What a control character means to the window rather than to the editor.
fn shortcut(key: &Key) -> Option<Shortcut> {
    match key {
        // A window reports Ctrl+C as the character or as the control code,
        // depending on the platform and the key.
        Key::Character(text) => match text.to_lowercase().as_str() {
            "c" | "\u{3}" => Some(Shortcut::Copy),
            "v" | "\u{16}" => Some(Shortcut::Paste),
            _ => None,
        },
        _ => None,
    }
}

/// The window's title: the file, when there is one.
fn title(file: Option<&Path>) -> String {
    match file {
        Some(path) => format!("inkstone — {}", path.display()),
        None => "inkstone".to_string(),
    }
}

/// The protocol message for a key press, mirroring the terminal frontend's
/// `to_key_msg`.
///
/// The rules are the terminal frontend's, in a window's vocabulary: Shift is
/// already baked into a character (`A` is `Char('A')`, not `Char('a')` with
/// `Shift`, or `A` and `<S-a>` would be two different keys), Ctrl and Alt become
/// modifiers, `<S-Tab>` is `Tab` with `Shift`, and a key the protocol cannot
/// express is dropped rather than guessed at.
fn to_key_msg(key: &Key, modifiers: ModifiersState) -> Option<KeyMsg> {
    let code = match key {
        Key::Character(text) => {
            let mut characters = text.chars();
            let character = match (characters.next(), characters.next()) {
                (Some(character), None) => character,
                // A composed cluster or a dead key's output is not one key the
                // protocol can express; the text path is for that.
                _ => return None,
            };
            KeyCode::Char(base_key(character, modifiers))
        }
        Key::Named(named) => to_key_code(*named)?,
        // A dead key on its own, and a key the platform could not name.
        Key::Dead(_) | Key::Unidentified(_) => return None,
    };

    let mut mods = Vec::new();
    if modifiers.control_key() {
        mods.push(Mod::Ctrl);
    }
    if modifiers.alt_key() {
        mods.push(Mod::Alt);
    }
    // Shift is only kept for the keys that have no character of their own.
    if modifiers.shift_key() && !matches!(code, KeyCode::Char(_)) {
        mods.push(Mod::Shift);
    }
    Some(KeyMsg { code, mods })
}

/// The protocol key for a named window key, if the protocol has one at all.
fn to_key_code(named: NamedKey) -> Option<KeyCode> {
    Some(match named {
        NamedKey::Enter => KeyCode::Enter,
        NamedKey::Escape => KeyCode::Esc,
        NamedKey::Backspace => KeyCode::Backspace,
        NamedKey::Tab => KeyCode::Tab,
        NamedKey::Space => KeyCode::Char(' '),
        NamedKey::ArrowLeft => KeyCode::Left,
        NamedKey::ArrowRight => KeyCode::Right,
        NamedKey::ArrowUp => KeyCode::Up,
        NamedKey::ArrowDown => KeyCode::Down,
        NamedKey::Home => KeyCode::Home,
        NamedKey::End => KeyCode::End,
        NamedKey::Delete => KeyCode::Delete,
        NamedKey::PageUp => KeyCode::PageUp,
        NamedKey::PageDown => KeyCode::PageDown,
        // Function keys, `Insert`, the modifiers themselves, and everything
        // else have no spelling in the protocol.
        _ => return None,
    })
}

/// The key a control character stands for.
///
/// A window may report `Ctrl-w` as the character it would type into a terminal
/// (`0x17`) rather than as `w` with Ctrl held. The protocol spells the latter,
/// which is also how the terminal frontend spells it, so the control character
/// is translated back. Anything without a printable key behind it is left as it
/// is: the protocol has no better spelling for it either.
fn base_key(character: char, modifiers: ModifiersState) -> char {
    if !modifiers.control_key() || !character.is_control() {
        return character;
    }
    match character {
        '\0' => ' ',
        '\u{1}'..='\u{1a}' => char::from(b'a' + (character as u8 - 1)),
        '\u{1b}' => '[',
        '\u{1c}'..='\u{1f}' => char::from(b'\\' + (character as u8 - 0x1c)),
        '\u{7f}' => '?',
        other => other,
    }
}

/// How many cells fit in a window of `size`, given the size of one cell in
/// physical pixels.
///
/// A cell that does not fit is not shown: the daemon is told about whole cells
/// only, so that what it lays out is what the window can draw. A window too
/// small for even one cell still gets one, because a zero-sized grid is not a
/// screen.
fn grid_size(size: PhysicalSize<u32>, cell: (f32, f32)) -> (u16, u16) {
    (
        cells(size.width as f32 / cell.0),
        cells(size.height as f32 / cell.1),
    )
}

/// A cell count as the daemon's grid spells it: at least one, and never more
/// than a `u16` can hold.
fn cells(count: f32) -> u16 {
    if !count.is_finite() || count < 1.0 {
        return 1;
    }
    if count >= f32::from(u16::MAX) {
        return u16::MAX;
    }
    count as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn character_key(character: &str, modifiers: ModifiersState) -> (Key, ModifiersState) {
        (Key::Character(character.into()), modifiers)
    }

    fn named_key(named: NamedKey, modifiers: ModifiersState) -> (Key, ModifiersState) {
        (Key::Named(named), modifiers)
    }

    #[test]
    fn shifted_characters_do_not_carry_a_shift_modifier() {
        let (key, modifiers) = character_key("A", ModifiersState::SHIFT);
        assert_eq!(
            to_key_msg(&key, modifiers),
            Some(KeyMsg::new(KeyCode::Char('A')))
        );
    }

    #[test]
    fn control_and_alt_are_forwarded() {
        let (key, modifiers) = character_key("w", ModifiersState::CONTROL);
        assert_eq!(to_key_msg(&key, modifiers), Some(KeyMsg::ctrl('w')));

        let (key, modifiers) = named_key(NamedKey::ArrowLeft, ModifiersState::ALT);
        assert_eq!(
            to_key_msg(&key, modifiers),
            Some(KeyMsg {
                code: KeyCode::Left,
                mods: vec![Mod::Alt]
            })
        );
    }

    #[test]
    fn a_control_character_is_the_letter_that_made_it() {
        // The same key press, reported the other way round: the window says
        // `0x17`, the protocol says `Ctrl-w`.
        let (key, modifiers) = character_key("\u{17}", ModifiersState::CONTROL);
        assert_eq!(to_key_msg(&key, modifiers), Some(KeyMsg::ctrl('w')));

        let (key, modifiers) = character_key("\u{1b}", ModifiersState::CONTROL);
        assert_eq!(to_key_msg(&key, modifiers), Some(KeyMsg::ctrl('[')));

        // Without Ctrl held a control character is left alone: guessing a key
        // for it would invent input.
        let (key, modifiers) = character_key("\u{17}", ModifiersState::empty());
        assert_eq!(
            to_key_msg(&key, modifiers),
            Some(KeyMsg::new(KeyCode::Char('\u{17}')))
        );
    }

    #[test]
    fn shift_is_kept_for_named_keys() {
        let (key, modifiers) = named_key(NamedKey::Tab, ModifiersState::SHIFT);
        assert_eq!(
            to_key_msg(&key, modifiers),
            Some(KeyMsg {
                code: KeyCode::Tab,
                mods: vec![Mod::Shift]
            })
        );
    }

    #[test]
    fn space_is_a_character_and_enter_is_not() {
        let (key, modifiers) = named_key(NamedKey::Space, ModifiersState::empty());
        assert_eq!(
            to_key_msg(&key, modifiers),
            Some(KeyMsg::new(KeyCode::Char(' ')))
        );

        let (key, modifiers) = named_key(NamedKey::Enter, ModifiersState::empty());
        assert_eq!(
            to_key_msg(&key, modifiers),
            Some(KeyMsg::new(KeyCode::Enter))
        );
    }

    #[test]
    fn keys_without_a_protocol_spelling_are_dropped() {
        let (key, modifiers) = named_key(NamedKey::F5, ModifiersState::empty());
        assert!(to_key_msg(&key, modifiers).is_none());
        let (key, modifiers) = named_key(NamedKey::Insert, ModifiersState::empty());
        assert!(to_key_msg(&key, modifiers).is_none());
        let (key, modifiers) = (Key::Dead(Some('^')), ModifiersState::empty());
        assert!(to_key_msg(&key, modifiers).is_none());
        // A cluster (`e` plus a combining acute) is text, not a key.
        let (key, modifiers) = character_key("e\u{301}", ModifiersState::empty());
        assert!(to_key_msg(&key, modifiers).is_none());
    }

    /// Iterating the protocol's key list means a new `KeyCode` cannot be added
    /// without deciding how a window produces it.
    #[test]
    fn every_protocol_key_has_a_window_mapping() {
        for &code in KeyCode::ALL {
            let (key, modifiers) = match code {
                KeyCode::Char(character) => {
                    character_key(&character.to_string(), ModifiersState::empty())
                }
                KeyCode::Enter => named_key(NamedKey::Enter, ModifiersState::empty()),
                KeyCode::Esc => named_key(NamedKey::Escape, ModifiersState::empty()),
                KeyCode::Backspace => named_key(NamedKey::Backspace, ModifiersState::empty()),
                KeyCode::Tab => named_key(NamedKey::Tab, ModifiersState::empty()),
                KeyCode::Left => named_key(NamedKey::ArrowLeft, ModifiersState::empty()),
                KeyCode::Right => named_key(NamedKey::ArrowRight, ModifiersState::empty()),
                KeyCode::Up => named_key(NamedKey::ArrowUp, ModifiersState::empty()),
                KeyCode::Down => named_key(NamedKey::ArrowDown, ModifiersState::empty()),
                KeyCode::Home => named_key(NamedKey::Home, ModifiersState::empty()),
                KeyCode::End => named_key(NamedKey::End, ModifiersState::empty()),
                KeyCode::Delete => named_key(NamedKey::Delete, ModifiersState::empty()),
                KeyCode::PageUp => named_key(NamedKey::PageUp, ModifiersState::empty()),
                KeyCode::PageDown => named_key(NamedKey::PageDown, ModifiersState::empty()),
            };
            assert_eq!(
                to_key_msg(&key, modifiers),
                Some(KeyMsg::new(code)),
                "{code:?} cannot be produced by the window"
            );
        }
    }

    #[test]
    fn a_window_is_as_many_cells_as_fit_in_it() {
        // The cell the font is sized for: 16 by 30 physical pixels.
        let cell = (16.0, 30.0);
        assert_eq!(grid_size(PhysicalSize::new(1024, 768), cell), (64, 25));
        assert_eq!(grid_size(PhysicalSize::new(800, 600), cell), (50, 20));
    }

    #[test]
    fn a_partial_cell_is_not_shown() {
        let cell = (16.0, 30.0);
        // One pixel short of a column and of a row.
        assert_eq!(grid_size(PhysicalSize::new(1023, 779), cell), (63, 25));
        assert_eq!(grid_size(PhysicalSize::new(1039, 809), cell), (64, 26));
    }

    #[test]
    fn a_window_smaller_than_a_cell_still_has_one() {
        let cell = (16.0, 30.0);
        assert_eq!(grid_size(PhysicalSize::new(1, 1), cell), (1, 1));
        assert_eq!(grid_size(PhysicalSize::new(0, 0), cell), (1, 1));
    }

    #[test]
    fn a_cell_of_no_width_does_not_divide_the_window_away() {
        // A cell of zero is not something the font code can produce, but the
        // arithmetic here must not turn it into an infinite grid.
        assert_eq!(grid_size(PhysicalSize::new(1024, 768), (0.0, 0.0)), (1, 1));
    }

    #[test]
    fn a_window_too_many_cells_across_is_clamped() {
        // Well past the width a `u16` column index can count.
        assert_eq!(
            grid_size(PhysicalSize::new(1_000_000, 1_000_000), (1.0, 1.0)),
            (u16::MAX, u16::MAX)
        );
    }

    #[test]
    fn a_pointer_position_becomes_a_cell() {
        // A 16 by 30 cell: the cell the pointer is in, not the nearest edge.
        assert_eq!(cell_at((0.0, 0.0), (16.0, 30.0)), Some((0, 0)));
        assert_eq!(cell_at((15.9, 29.9), (16.0, 30.0)), Some((0, 0)));
        assert_eq!(cell_at((16.0, 30.0), (16.0, 30.0)), Some((1, 1)));
        assert_eq!(cell_at((320.0, 300.0), (16.0, 30.0)), Some((20, 10)));
        // A cell of no size, or a position that is not somewhere, is nothing.
        assert_eq!(cell_at((10.0, 10.0), (0.0, 30.0)), None);
        assert_eq!(cell_at((-1.0, 10.0), (16.0, 30.0)), None);
        assert_eq!(cell_at((f64::NAN, 0.0), (16.0, 30.0)), None);
    }

    #[test]
    fn control_c_and_v_are_the_windows_own() {
        assert_eq!(
            shortcut(&Key::Character("c".into())),
            Some(Shortcut::Copy),
            "the character a window reports for Ctrl+C"
        );
        assert_eq!(
            shortcut(&Key::Character("\u{3}".into())),
            Some(Shortcut::Copy),
            "and the control code the other platforms report"
        );
        assert_eq!(shortcut(&Key::Character("v".into())), Some(Shortcut::Paste));
        assert_eq!(
            shortcut(&Key::Character("\u{16}".into())),
            Some(Shortcut::Paste)
        );
        assert_eq!(shortcut(&Key::Character("x".into())), None);
        assert_eq!(shortcut(&Key::Named(NamedKey::Escape)), None);
    }

    #[test]
    fn the_title_names_the_file_when_there_is_one() {
        assert_eq!(title(None), "inkstone");
        assert_eq!(title(Some(Path::new("notes.txt"))), "inkstone — notes.txt");
    }
}
