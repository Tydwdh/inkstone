//! Wire protocol for `inkstone`.
//!
//! The protocol is a stream of length-prefixed MessagePack frames. Each frame is
//! a `u32` big-endian byte length followed by that many bytes of MessagePack
//! payload. Two symmetric message families travel over the same stream:
//!
//! * [`ClientMsg`] — frontend → daemon (input events, resizes, commands).
//! * [`ServerMsg`] — daemon → frontend (screen updates, quit).
//!
//! The screen goes out whole once, and as the rows that changed after that, so a
//! frontend folds every frame it reads into the screen it is showing:
//! [`Screen`] is that fold, and the only state a frontend keeps.
//!
//! Milestone 1 is notification-only: there are no request ids and no replies.
//!
//! This crate is also the single definition site of the screen model
//! ([`Grid`], [`Cell`], [`Color`]) so that the daemon and every frontend agree
//! on it byte-for-byte.

#![warn(missing_docs)]

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

mod macros;

/// Hard cap on a single frame, guarding against corrupt/hostile length prefixes.
pub const MAX_FRAME_LEN: u32 = 64 * 1024 * 1024;

cell_attributes! {
    /// Bold text.
    ATTR_BOLD = 0,
    /// Underlined text.
    ATTR_UNDERLINE = 1,
    /// Foreground and background swapped.
    ATTR_REVERSE = 2,
    /// Slanted text.
    ATTR_ITALIC = 3,
    /// Dimmer text, for things that are there but not the point.
    ATTR_FAINT = 4,
    /// Struck-through text.
    ATTR_STRIKETHROUGH = 5,
}

/// Errors produced while encoding, decoding, or framing a message.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The Lua VM could not be set up, or a chunk failed to run.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    /// A message could not be encoded.
    #[error("failed to encode message: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    /// A frame could not be decoded.
    #[error("failed to decode message: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    /// A length prefix claimed more than [`MAX_FRAME_LEN`] bytes, which means
    /// the stream is corrupt or hostile.
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_LEN} byte limit")]
    FrameTooLarge(u32),
}

/// What this crate's functions return.
pub type Result<T, E = RpcError> = std::result::Result<T, E>;

// ---------------------------------------------------------------------------
// Input events
// ---------------------------------------------------------------------------

key_codes! {
    data {
        /// A character key. Its notation is the character itself, which is why
        /// it declares a representative rather than spellings.
        Char(char) => 'a',
    }
    keys {
        /// Enter / Return.
        Enter => "CR" | "Enter" | "Return",
        /// Escape.
        Esc => "Esc" | "Escape",
        /// Backspace.
        Backspace => "BS" | "Backspace",
        /// Tab.
        Tab => "Tab",
        /// The left arrow.
        Left => "Left",
        /// The right arrow.
        Right => "Right",
        /// The up arrow.
        Up => "Up",
        /// The down arrow.
        Down => "Down",
        /// Home.
        Home => "Home",
        /// End.
        End => "End",
        /// Delete forward.
        Delete => "Del" | "Delete",
        /// Page up.
        PageUp => "PageUp",
        /// Page down.
        PageDown => "PageDown",
    }
}

/// A modifier held while a key was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mod {
    /// Control.
    Ctrl,
    /// Alt, or Option on a Mac.
    Alt,
    /// Shift.
    Shift,
}

/// A key press: a key code plus the modifiers that were down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyMsg {
    /// Which key.
    pub code: KeyCode,
    /// Modifiers that were held while it was pressed.
    pub mods: Vec<Mod>,
}

impl KeyMsg {
    /// The key on its own, with no modifiers.
    pub fn new(code: KeyCode) -> Self {
        Self {
            code,
            mods: Vec::new(),
        }
    }

    /// A character key with Control held.
    pub fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            mods: vec![Mod::Ctrl],
        }
    }

    /// Whether Control was held.
    pub fn is_ctrl(&self) -> bool {
        self.mods.contains(&Mod::Ctrl)
    }

    /// Whether Alt was held.
    pub fn is_alt(&self) -> bool {
        self.mods.contains(&Mod::Alt)
    }
}

/// What the pointer did, and where, in screen cells.
///
/// A frontend sends cells, never buffer positions: which line and column a cell
/// is depends on the gutter, the tabstops, the wide graphemes and the horizontal
/// scroll, all of which are the daemon's business.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerMsg {
    /// What happened.
    pub kind: PointerKind,
    /// The screen column, zero-based from the left edge.
    pub x: u16,
    /// The screen row, zero-based from the top.
    pub y: u16,
}

impl PointerMsg {
    /// An event at `(x, y)`.
    pub fn new(kind: PointerKind, x: u16, y: u16) -> Self {
        Self { kind, x, y }
    }
}

/// What a pointer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerKind {
    /// Pressed: put the cursor here, and start a selection.
    Press,
    /// Moved with the button down: extend the selection to here.
    Drag,
    /// Released: a press that never dragged was only a click.
    Release,
    /// The wheel, upwards.
    ScrollUp,
    /// The wheel, downwards.
    ScrollDown,
}

/// A scalar argument to a programmatic command invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Param {
    /// No value. Also how a script says "unbind" to `ed.map`.
    Nil,
    /// A boolean.
    Bool(bool),
    /// An integer. A command treats a positive first parameter as a repeat count.
    Int(i64),
    /// A string, usually a path or a command name.
    Str(String),
}

impl From<bool> for Param {
    fn from(v: bool) -> Self {
        Param::Bool(v)
    }
}

impl From<i64> for Param {
    fn from(v: i64) -> Self {
        Param::Int(v)
    }
}

impl From<String> for Param {
    fn from(v: String) -> Self {
        Param::Str(v)
    }
}

impl From<&str> for Param {
    fn from(v: &str) -> Self {
        Param::Str(v.to_owned())
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Frontend → daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// A key was pressed.
    Input(KeyMsg),
    /// The pointer did something, in screen cells.
    Pointer(PointerMsg),
    /// Put the text the editor is holding on the system clipboard.
    ///
    /// The visual selection if one is up, the register otherwise: the daemon
    /// answers with [`ServerMsg::Clipboard`], and the frontend writes it where
    /// text goes — the OS clipboard in a window, OSC 52 in a terminal.
    Copy,
    /// Text arrived that was never a key: a paste, or what an input method
    /// committed.
    ///
    /// It is inserted at the cursor as one change, the way typing it would have
    /// been, and it is one step to undo. Line endings are kept exactly as they
    /// came, so pasting CRLF into a file that uses LF puts CRLF in it — the same
    /// thing a terminal paste does.
    Text(String),
    /// The terminal changed size.
    Resize {
        /// New width in columns.
        cols: u16,
        /// New height in rows.
        rows: u16,
    },
    /// Run a command by name, as if it had been typed.
    Command {
        /// The command to run.
        name: String,
        /// Its arguments.
        params: Vec<Param>,
    },
    /// The frontend is going away; end the session.
    Shutdown,
}

/// A terminal color, mirroring the three common terminal color spaces.
///
/// It travels as one integer — see [`Color::packed`] — because the screen is
/// re-sent on every keystroke and a plain `enum` would spend its *variant name*
/// on the wire: six bytes for the default colour, and twelve for the two
/// colours in every blank cell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u32", try_from = "u32")]
pub enum Color {
    /// The terminal's default foreground/background.
    #[default]
    Reset,
    /// An entry in the 256-color palette.
    Indexed(u8),
    /// A 24-bit true color.
    Rgb(u8, u8, u8),
}

/// The kind of a colour, in the low two bits of its packed form.
const COLOR_KIND_MASK: u32 = 0b11;

/// What went wrong unpacking a colour off the wire.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown color kind in {0:#x}")]
pub struct BadColor(pub u32);

impl Color {
    /// This colour as one integer: kind in the low two bits, payload above it.
    ///
    /// `Reset` is `0`, an indexed colour is its index shifted left, and a true
    /// color is its 24 bits shifted left — which MessagePack then writes as one
    /// or five bytes instead of the nine a three-field variant needs.
    pub fn packed(self) -> u32 {
        match self {
            Color::Reset => 0,
            Color::Indexed(i) => (u32::from(i) << 2) | 1,
            Color::Rgb(r, g, b) => {
                (u32::from(r) << 18 | u32::from(g) << 10 | u32::from(b) << 2) | 2
            }
        }
    }

    /// The colour `packed` describes.
    ///
    /// Errors when its kind bits are not a kind this protocol has, or when its
    /// payload does not fit the field that kind belongs in.
    pub fn unpacked(packed: u32) -> Result<Self, BadColor> {
        match packed & COLOR_KIND_MASK {
            0 => Ok(Color::Reset),
            1 => match u8::try_from(packed >> 2) {
                Ok(index) => Ok(Color::Indexed(index)),
                Err(_) => Err(BadColor(packed)),
            },
            2 => Ok(Color::Rgb(
                ((packed >> 18) & 0xff) as u8,
                ((packed >> 10) & 0xff) as u8,
                ((packed >> 2) & 0xff) as u8,
            )),
            _ => Err(BadColor(packed)),
        }
    }
}

impl From<Color> for u32 {
    fn from(color: Color) -> Self {
        color.packed()
    }
}

impl TryFrom<u32> for Color {
    type Error = BadColor;

    fn try_from(packed: u32) -> Result<Self, Self::Error> {
        Color::unpacked(packed)
    }
}

/// One character cell of the screen.
///
/// `text` holds a single grapheme cluster (usually one `char`). A cell whose
/// `text` is empty is a *continuation cell*: it is the second half of a
/// double-width grapheme that lives in the cell to its left. Frontends must
/// skip continuation cells when drawing. Every other cell holds the glyph that
/// belongs there — including a plain `" "` for empty screen space — so a full
/// grid is always self-describing and needs no clearing beforehand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// The glyph here, one grapheme cluster. Empty means a continuation cell;
    /// see [`Cell::is_continuation`].
    pub text: String,
    /// Foreground colour.
    pub fg: Color,
    /// Background colour.
    pub bg: Color,
    /// Attribute bits, from [`ATTRS`].
    pub attrs: u16,
}

impl Cell {
    /// An empty cell, which on screen means a continuation cell.
    pub fn blank() -> Self {
        Self {
            text: String::new(),
            fg: Color::Reset,
            bg: Color::Reset,
            attrs: 0,
        }
    }

    /// A cell showing `text`.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Cell::blank()
        }
    }

    /// Set the foreground colour.
    pub fn with_fg(mut self, fg: Color) -> Self {
        self.fg = fg;
        self
    }

    /// Set the background colour.
    pub fn with_bg(mut self, bg: Color) -> Self {
        self.bg = bg;
        self
    }

    /// Set the attribute bits.
    pub fn with_attrs(mut self, attrs: u16) -> Self {
        self.attrs = attrs;
        self
    }

    /// True when this cell is a continuation cell (no glyph of its own).
    pub fn is_continuation(&self) -> bool {
        self.text.is_empty()
    }
}

/// One row of the screen that changed, as runs of identical cells.
///
/// A screen is mostly long stretches of the same cell — the blanks after the
/// last character of a line, the reverse-video padding of the status row — and a
/// run says that in a few bytes instead of one cell per column. A terminal
/// frontend prints `count` copies of `text` in one style; a GPU frontend turns
/// each run into a text quad.
///
/// The runs cover the whole row, so a frontend never has to decide what the
/// cells it was not sent should be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowUpdate {
    /// Which row, counted from the top of the screen.
    pub y: u16,
    /// The row, in order, covering every column.
    pub runs: Vec<CellRun>,
}

/// `count` identical cells, next to each other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellRun {
    /// How many cells this run stands for. Never zero.
    pub count: u16,
    /// The cell they all are.
    pub cell: Cell,
}

impl RowUpdate {
    /// The update that takes row `y` to `cells`.
    pub fn new(y: u16, cells: &[Cell]) -> Self {
        let mut runs: Vec<CellRun> = Vec::with_capacity(cells.len());
        for cell in cells {
            match runs.last_mut() {
                Some(run) if run.cell == *cell => run.count += 1,
                _ => runs.push(CellRun {
                    count: 1,
                    cell: cell.clone(),
                }),
            }
        }
        Self { y, runs }
    }

    /// Write this update into `grid`.
    pub fn apply_to(&self, grid: &mut Grid) {
        let mut x = 0;
        for run in &self.runs {
            for _ in 0..run.count {
                grid.put(x, self.y, run.cell.clone());
                x += 1;
            }
        }
    }
}

/// A row-major grid of [`Cell`]s: the entire screen, as computed by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    /// Screen columns.
    pub width: u16,
    /// Screen rows.
    pub height: u16,
    /// `width * height` cells, row-major.
    pub cells: Vec<Cell>,
}

impl Grid {
    /// A grid of blank cells, which is to say spaces.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            cells: vec![Cell::new(" "); width as usize * height as usize],
        }
    }

    /// Whether `(x, y)` is inside the grid.
    pub fn in_bounds(&self, x: u16, y: u16) -> bool {
        x < self.width && y < self.height
    }

    /// The cell at `(x, y)`. Panics when out of bounds.
    pub fn get(&self, x: u16, y: u16) -> &Cell {
        &self.cells[y as usize * self.width as usize + x as usize]
    }

    /// Replace the cell at `(x, y)`. Out-of-bounds writes are ignored, which
    /// is what lets the renderer draw wide graphemes without edge cases.
    pub fn put(&mut self, x: u16, y: u16, cell: Cell) {
        if self.in_bounds(x, y) {
            self.cells[y as usize * self.width as usize + x as usize] = cell;
        }
    }

    /// Row `y`, or an empty slice when the row is outside the grid.
    pub fn row(&self, y: u16) -> &[Cell] {
        let start = y as usize * self.width as usize;
        match self.cells.get(start..start + self.width as usize) {
            Some(row) => row,
            None => &[],
        }
    }

    /// The rows that differ from `previous`, trimmed.
    ///
    /// This is the daemon's half of a delta frame: it is empty when only the
    /// cursor moved, which is what most keystrokes do. A row `previous` does
    /// not have counts as changed, so a grid that changed shape needs no
    /// special case here.
    pub fn rows_changed_since(&self, previous: &Grid) -> Vec<RowUpdate> {
        (0..self.height)
            .filter(|&y| self.row(y) != previous.row(y))
            .map(|y| RowUpdate::new(y, self.row(y)))
            .collect()
    }

    /// Write a grapheme known to be `width` columns wide at `(x, y)`.
    ///
    /// When `width == 2` the following cell becomes a continuation cell.
    pub fn put_grapheme(&mut self, x: u16, y: u16, width: usize, cell: Cell) {
        match width {
            0 | 1 => self.put(x, y, cell),
            _ => {
                // The cells the grapheme spills into keep its colours and its
                // attributes, with no text of their own: a highlight that covers
                // a wide character has to cover both halves of it.
                let continuation = Cell {
                    text: String::new(),
                    ..cell.clone()
                };
                self.put(x, y, cell);
                self.put(x + 1, y, continuation);
            }
        }
    }

    /// The text of row `y`, with continuation cells elided.
    pub fn row_text(&self, y: u16) -> String {
        let mut out = String::new();
        for x in 0..self.width {
            let cell = self.get(x, y);
            if cell.is_continuation() {
                continue;
            }
            out.push_str(&cell.text);
        }
        out
    }
}

/// The screen a frontend is holding.
///
/// The daemon sends the whole screen once and the rows that changed after that,
/// so a frontend that keeps this in step with the stream always has the screen
/// it should be showing. It caches the *screen* and nothing else: no document,
/// no editor cursor, no modes — none of that is on this side of the wire.
///
/// Until the first full frame the grid is empty (0 by 0), so a row update lands
/// nowhere and the screen stays blank. The daemon always opens with a full
/// frame, so that can only happen to a peer that does not.
#[derive(Debug, Clone)]
pub struct Screen {
    grid: Grid,
    cursor: (u16, u16),
    cursor_visible: bool,
    insert: bool,
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen {
    /// An empty screen: nothing has been drawn yet.
    pub fn new() -> Self {
        Self {
            grid: Grid::new(0, 0),
            cursor: (0, 0),
            cursor_visible: false,
            insert: false,
        }
    }

    /// The grid to draw.
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Where the terminal cursor belongs, in screen coordinates.
    pub fn cursor(&self) -> (u16, u16) {
        self.cursor
    }

    /// Whether to show the cursor at all.
    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Whether the editor is inserting text: the cursor is a bar here, and an
    /// input method belongs here and nowhere else.
    pub fn insert(&self) -> bool {
        self.insert
    }

    /// Take a whole frame, and return every row as one that needs drawing.
    pub fn redraw(
        &mut self,
        grid: Grid,
        cursor: (u16, u16),
        cursor_visible: bool,
        insert: bool,
    ) -> Vec<u16> {
        self.grid = grid;
        self.cursor = cursor;
        self.cursor_visible = cursor_visible;
        self.insert = insert;
        (0..self.grid.height).collect()
    }

    /// Take the rows that changed, and return the rows that need drawing: the
    /// ones that changed, or none when only the cursor moved.
    ///
    /// The rows are written into the screen as they are applied, so the grid
    /// here and the daemon's grid can only disagree if a frame was dropped.
    pub fn rows(
        &mut self,
        updates: &[RowUpdate],
        cursor: (u16, u16),
        cursor_visible: bool,
        insert: bool,
    ) -> Vec<u16> {
        self.cursor = cursor;
        self.cursor_visible = cursor_visible;
        self.insert = insert;
        updates
            .iter()
            .map(|update| {
                update.apply_to(&mut self.grid);
                update.y
            })
            .collect()
    }
}

/// Daemon → frontend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg {
    /// The whole screen. A frontend only ever needs to blit this.
    Redraw {
        /// What to draw.
        grid: Grid,
        /// Where the terminal cursor belongs, in screen coordinates.
        cursor: (u16, u16),
        /// Whether to show the cursor at all.
        cursor_visible: bool,
        /// Whether the editor is inserting text: draw the cursor as a bar and
        /// let an input method in, because that is where text goes. Everywhere
        /// else it is a block and the keyboard belongs to the editor.
        insert: bool,
    },
    /// Rows that changed since the frame before this one.
    ///
    /// Only ever sent against the previous frame, and never as the first frame
    /// of a session or after a resize: the daemon sends a full [`ServerMsg::Redraw`]
    /// when it has nothing to compare against. `updates` is empty when only the
    /// cursor moved, which is what most keystrokes do.
    Rows {
        /// The rows that changed, in any order, each as runs.
        updates: Vec<RowUpdate>,
        /// Where the terminal cursor belongs, in screen coordinates.
        cursor: (u16, u16),
        /// Whether to show the cursor at all.
        cursor_visible: bool,
        /// Whether the editor is inserting text; see [`ServerMsg::Redraw`].
        insert: bool,
    },
    /// Text the frontend should put on the system clipboard, exactly as the
    /// buffer holds it.
    Clipboard(String),
    /// The session is over; the frontend should restore the terminal and exit.
    Quit,
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Encode `msg` and write it as one length-prefixed frame.
///
/// MessagePack's *compact* form (`to_vec`), not the named one: a struct becomes
/// an array of its fields instead of a map keyed by field names, which for a
/// full screen is worth about 5x (63 KiB → 12 KiB for 80x24 of cells, and the
/// grid is sent again on every keystroke). Both sides of the wire are in this
/// workspace and the round-trip tests cover it, so the names buy nothing.
pub fn write_msg<W, M>(w: &mut W, msg: &M) -> Result<()>
where
    W: Write,
    M: Serialize,
{
    let payload = rmp_serde::to_vec(msg)?;
    let len = u32::try_from(payload.len()).map_err(|_| RpcError::FrameTooLarge(u32::MAX))?;
    if len > MAX_FRAME_LEN {
        return Err(RpcError::FrameTooLarge(len));
    }
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&payload)?;
    Ok(())
}

/// Read one length-prefixed frame and decode it.
///
/// Returns `Ok(None)` on a clean end of stream (no bytes of the length prefix
/// were readable), which is how a peer signals that it is done.
pub fn read_msg<R, M>(r: &mut R) -> Result<Option<M>>
where
    R: Read,
    M: DeserializeOwned,
{
    let mut prefix = [0u8; 4];
    if !read_exact_or_eof(r, &mut prefix)? {
        return Ok(None);
    }
    let len = u32::from_be_bytes(prefix);
    if len > MAX_FRAME_LEN {
        return Err(RpcError::FrameTooLarge(len));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    let msg = rmp_serde::from_slice(&payload)?;
    Ok(Some(msg))
}

// Typed convenience wrappers.

/// Write one frontend-to-daemon frame.
pub fn write_client_msg<W: Write>(w: &mut W, msg: &ClientMsg) -> Result<()> {
    write_msg(w, msg)
}

/// Write one daemon-to-frontend frame.
pub fn write_server_msg<W: Write>(w: &mut W, msg: &ServerMsg) -> Result<()> {
    write_msg(w, msg)
}

/// Read one frontend-to-daemon frame. `Ok(None)` means the peer went away.
pub fn read_client_msg<R: Read>(r: &mut R) -> Result<Option<ClientMsg>> {
    read_msg(r)
}

/// Read one daemon-to-frontend frame. `Ok(None)` means the peer went away.
pub fn read_server_msg<R: Read>(r: &mut R) -> Result<Option<ServerMsg>> {
    read_msg(r)
}

/// Fill `buf` completely. Returns `Ok(false)` if EOF was hit before any byte.
fn read_exact_or_eof<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(false);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream ended in the middle of a frame header",
                ));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A reader that yields at most one byte per `read`, to prove the framing
    /// loop tolerates arbitrarily fragmented streams.
    struct Trickle<'a> {
        inner: &'a [u8],
        pos: usize,
    }

    impl<'a> Read for Trickle<'a> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.inner.len() || buf.is_empty() {
                return Ok(0);
            }
            buf[0] = self.inner[self.pos];
            self.pos += 1;
            Ok(1)
        }
    }

    fn client_samples() -> Vec<ClientMsg> {
        vec![
            ClientMsg::Input(KeyMsg::new(KeyCode::Esc)),
            ClientMsg::Input(KeyMsg::ctrl('w')),
            ClientMsg::Input(KeyMsg {
                code: KeyCode::Char('é'),
                mods: vec![Mod::Alt, Mod::Shift],
            }),
            ClientMsg::Text("好\nsecond line\r\n".to_string()),
            ClientMsg::Pointer(PointerMsg::new(PointerKind::Press, 3, 4)),
            ClientMsg::Pointer(PointerMsg::new(PointerKind::ScrollDown, 0, 0)),
            ClientMsg::Copy,
            ClientMsg::Resize {
                cols: 120,
                rows: 40,
            },
            ClientMsg::Command {
                name: "move_right".into(),
                params: vec![Param::Int(3), Param::Str("x".into()), Param::Bool(true)],
            },
            ClientMsg::Shutdown,
        ]
    }

    fn server_samples() -> Vec<ServerMsg> {
        let mut grid = Grid::new(4, 2);
        grid.put(0, 0, Cell::new("a"));
        grid.put(1, 0, Cell::new("好").with_fg(Color::Rgb(1, 2, 3)));
        grid.put(2, 0, Cell::blank());
        grid.put(
            3,
            0,
            Cell::new("b")
                .with_bg(Color::Indexed(7))
                .with_attrs(ATTR_REVERSE),
        );
        vec![
            ServerMsg::Redraw {
                grid,
                cursor: (3, 1),
                cursor_visible: true,
                insert: false,
            },
            ServerMsg::Rows {
                updates: vec![RowUpdate::new(1, &[Cell::new("好"), Cell::blank()])],
                cursor: (0, 0),
                cursor_visible: false,
                insert: true,
            },
            ServerMsg::Clipboard("copied\ntext\n".to_string()),
            ServerMsg::Quit,
        ]
    }

    #[test]
    fn a_row_becomes_runs_and_comes_back_whole() {
        let mut grid = Grid::new(6, 1);
        grid.put(0, 0, Cell::new("a"));
        grid.put(1, 0, Cell::new("好"));
        grid.put(2, 0, Cell::blank()); // the second half of the wide glyph

        let update = RowUpdate::new(0, grid.row(0));
        assert_eq!(update.y, 0);
        assert_eq!(
            update.runs.len(),
            4,
            "a, the wide glyph, its continuation, then the blanks: {:?}",
            update.runs
        );
        assert_eq!(update.runs[3].count, 3, "the trailing blanks are one run");

        // Putting the update back into a blank screen reproduces the row.
        let mut screen = Grid::new(6, 1);
        update.apply_to(&mut screen);
        assert_eq!(screen, grid);
    }

    #[test]
    fn a_row_of_blanks_is_one_run() {
        let grid = Grid::new(200, 1);
        let update = RowUpdate::new(0, grid.row(0));
        assert_eq!(update.runs.len(), 1);
        assert_eq!(update.runs[0].count, 200);

        let mut screen = Grid::new(200, 1);
        update.apply_to(&mut screen);
        assert_eq!(screen, grid);
    }

    /// The contract of a delta frame: changed rows only, and applying them to
    /// the previous screen gives the new one exactly.
    #[test]
    fn a_wide_grapheme_styles_the_cells_it_covers() {
        let mut grid = Grid::new(4, 1);
        grid.put_grapheme(0, 0, 2, Cell::new("好").with_attrs(ATTR_REVERSE));
        assert_eq!(grid.get(1, 0).text, "", "the continuation has no glyph");
        assert_eq!(
            grid.get(1, 0).attrs & ATTR_REVERSE,
            ATTR_REVERSE,
            "but it carries the same highlight"
        );
    }

    #[test]
    fn only_the_rows_that_changed_come_back() {
        let mut before = Grid::new(4, 3);
        before.put(0, 1, Cell::new("x"));
        let mut after = before.clone();
        after.put(1, 1, Cell::new("y"));
        after.put(3, 2, Cell::new("z"));

        let updates = after.rows_changed_since(&before);
        assert_eq!(
            updates.iter().map(|u| u.y).collect::<Vec<_>>(),
            vec![1, 2],
            "rows 0 and 1 are one update each"
        );

        let mut screen = before.clone();
        for update in &updates {
            update.apply_to(&mut screen);
        }
        assert_eq!(screen, after);
        assert!(
            after.rows_changed_since(&after).is_empty(),
            "moving the cursor is not a change to the screen"
        );

        // A grid that changed shape has no row in common to compare against, so
        // every one of its rows comes back. (The daemon sends a whole frame
        // instead in that case; this is what makes the diff total anyway.)
        let wider = Grid::new(6, 3);
        let updates = wider.rows_changed_since(&Grid::new(4, 2));
        assert_eq!(updates.len(), 3, "every row of the wider grid");
    }

    #[test]
    fn colours_pack_into_one_integer() {
        for color in [
            Color::Reset,
            Color::Indexed(0),
            Color::Indexed(7),
            Color::Indexed(255),
            Color::Rgb(0, 0, 0),
            Color::Rgb(1, 2, 3),
            Color::Rgb(255, 255, 255),
        ] {
            assert_eq!(
                Color::unpacked(color.packed()),
                Ok(color),
                "{color:?} does not survive its own packing"
            );
        }
        // Kind 3 is not a kind, and an index that does not fit in a byte is not
        // a colour: both are refused rather than silently truncated.
        assert_eq!(Color::unpacked(0b11), Err(BadColor(0b11)));
        assert_eq!(
            Color::unpacked((0x100 << 2) | 1),
            Err(BadColor((0x100 << 2) | 1))
        );
        // One MessagePack byte for the default colour, at most five for any
        // colour the protocol can express.
        assert_eq!(rmp_serde::to_vec(&Color::Reset).unwrap().len(), 1);
        assert!(rmp_serde::to_vec(&Color::Rgb(255, 255, 255)).unwrap().len() <= 5);
    }

    /// A whole screen travels on every keystroke, so the size of one is a cost
    /// the protocol has to keep down. This pins the compact encoding and the
    /// packed colours against a regression back to per-field and per-variant
    /// names — it is a guard, not a benchmark.
    #[test]
    fn a_full_screen_frame_stays_small() {
        let mut grid = Grid::new(80, 24);
        for y in 0..grid.height {
            for (x, c) in "the quick brown fox".chars().enumerate() {
                grid.put(x as u16, y, Cell::new(c.to_string()).with_attrs(ATTR_BOLD));
            }
        }
        let mut buf = Vec::new();
        write_server_msg(
            &mut buf,
            &ServerMsg::Redraw {
                grid,
                cursor: (0, 0),
                cursor_visible: true,
                insert: false,
            },
        )
        .unwrap();
        // 11.6 KiB as it stands; 63 KiB while a field name and a colour name
        // were written out for every cell.
        assert!(
            buf.len() < 16 * 1024,
            "an 80x24 frame is {} bytes",
            buf.len()
        );
    }

    #[test]
    fn client_messages_round_trip() {
        for msg in client_samples() {
            let mut buf = Vec::new();
            write_msg(&mut buf, &msg).unwrap();
            let decoded: ClientMsg = read_msg(&mut Cursor::new(&buf)).unwrap().unwrap();
            assert_eq!(msg, decoded);
        }
    }

    #[test]
    fn server_messages_round_trip() {
        for msg in server_samples() {
            let mut buf = Vec::new();
            write_server_msg(&mut buf, &msg).unwrap();
            let decoded = read_server_msg(&mut Cursor::new(&buf)).unwrap().unwrap();
            assert_eq!(msg, decoded);
        }
    }

    #[test]
    fn multiple_messages_in_one_stream() {
        let mut buf = Vec::new();
        for msg in server_samples() {
            write_server_msg(&mut buf, &msg).unwrap();
        }
        let mut cur = Cursor::new(&buf);
        let mut got = Vec::new();
        while let Some(m) = read_server_msg(&mut cur).unwrap() {
            got.push(m);
        }
        assert_eq!(got, server_samples());
    }

    #[test]
    fn fragmented_stream_is_reassembled() {
        for msg in client_samples() {
            let mut buf = Vec::new();
            write_msg(&mut buf, &msg).unwrap();
            let mut trickle = Trickle {
                inner: &buf,
                pos: 0,
            };
            let decoded: ClientMsg = read_msg(&mut trickle).unwrap().unwrap();
            assert_eq!(msg, decoded);
        }
    }

    #[test]
    fn empty_stream_reads_as_none() {
        let mut cur = Cursor::new(Vec::<u8>::new());
        assert_eq!(read_client_msg(&mut cur).unwrap(), None);
        assert_eq!(read_server_msg(&mut cur).unwrap(), None);
    }

    #[test]
    fn truncated_header_is_an_error() {
        let buf = vec![0u8, 0, 0, 9];
        let mut cur = Cursor::new(buf);
        let err = read_client_msg(&mut cur).unwrap_err();
        assert!(matches!(err, RpcError::Io(_)), "got {err:?}");
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let buf = (MAX_FRAME_LEN + 1).to_be_bytes().to_vec();
        let mut cur = Cursor::new(buf);
        let err = read_client_msg(&mut cur).unwrap_err();
        assert!(matches!(err, RpcError::FrameTooLarge(_)), "got {err:?}");
    }

    #[test]
    fn grid_geometry_helpers() {
        let mut grid = Grid::new(3, 1);
        assert_eq!(grid.row_text(0), "   ", "a fresh grid is spaces, not holes");
        grid.put_grapheme(0, 0, 2, Cell::new("好"));
        grid.put(2, 0, Cell::new("z"));
        assert!(grid.get(1, 0).is_continuation());
        assert_eq!(grid.row_text(0), "好z");

        // A grid is a value: replacing it is a plain assignment.
        let bigger = Grid::new(2, 2);
        assert_eq!(bigger.width, 2);
        assert_eq!(bigger.height, 2);
        assert_eq!(bigger.row_text(0), "  ");
    }
}
