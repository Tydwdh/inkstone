//! Wire protocol for `edit`.
//!
//! The protocol is a stream of length-prefixed MessagePack frames. Each frame is
//! a `u32` big-endian byte length followed by that many bytes of MessagePack
//! payload. Two symmetric message families travel over the same stream:
//!
//! * [`ClientMsg`] — frontend → daemon (input events, resizes, commands).
//! * [`ServerMsg`] — daemon → frontend (screen updates, quit).
//!
//! Milestone 1 is notification-only: there are no request ids and no replies.
//!
//! This crate is also the single definition site of the screen model
//! ([`Grid`], [`Cell`], [`Color`]) so that the daemon and every frontend agree
//! on it byte-for-byte.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Hard cap on a single frame, guarding against corrupt/hostile length prefixes.
pub const MAX_FRAME_LEN: u32 = 64 * 1024 * 1024;

/// Cell attribute bit flags.
pub const ATTR_BOLD: u16 = 1 << 0;
pub const ATTR_UNDERLINE: u16 = 1 << 1;
pub const ATTR_REVERSE: u16 = 1 << 2;

/// Errors produced while encoding, decoding, or framing a message.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("failed to encode message: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("failed to decode message: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_LEN} byte limit")]
    FrameTooLarge(u32),
}

pub type Result<T, E = RpcError> = std::result::Result<T, E>;

// ---------------------------------------------------------------------------
// Input events
// ---------------------------------------------------------------------------

/// A key, decoupled from any particular terminal library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Delete,
    PageUp,
    PageDown,
}

/// A modifier held while a key was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mod {
    Ctrl,
    Alt,
    Shift,
}

/// A key press: a key code plus the modifiers that were down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyMsg {
    pub code: KeyCode,
    pub mods: Vec<Mod>,
}

impl KeyMsg {
    pub fn new(code: KeyCode) -> Self {
        Self {
            code,
            mods: Vec::new(),
        }
    }

    pub fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            mods: vec![Mod::Ctrl],
        }
    }

    pub fn is_ctrl(&self) -> bool {
        self.mods.contains(&Mod::Ctrl)
    }

    pub fn is_alt(&self) -> bool {
        self.mods.contains(&Mod::Alt)
    }
}

/// A scalar argument to a programmatic command invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Param {
    Nil,
    Bool(bool),
    Int(i64),
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
    Input(KeyMsg),
    Resize { cols: u16, rows: u16 },
    Command { name: String, params: Vec<Param> },
    Shutdown,
}

/// A terminal color, mirroring the three common terminal color spaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    /// The terminal's default foreground/background.
    #[default]
    Reset,
    /// An entry in the 256-color palette.
    Indexed(u8),
    /// A 24-bit true color.
    Rgb(u8, u8, u8),
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
    pub text: String,
    pub fg: Color,
    pub bg: Color,
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

    pub fn with_fg(mut self, fg: Color) -> Self {
        self.fg = fg;
        self
    }

    pub fn with_bg(mut self, bg: Color) -> Self {
        self.bg = bg;
        self
    }

    pub fn with_attrs(mut self, attrs: u16) -> Self {
        self.attrs = attrs;
        self
    }

    /// True when this cell is a continuation cell (no glyph of its own).
    pub fn is_continuation(&self) -> bool {
        self.text.is_empty()
    }
}

/// A row-major grid of [`Cell`]s: the entire screen, as computed by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    pub width: u16,
    pub height: u16,
    /// `width * height` cells, row-major.
    pub cells: Vec<Cell>,
}

impl Grid {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            cells: vec![Cell::new(" "); width as usize * height as usize],
        }
    }

    pub fn in_bounds(&self, x: u16, y: u16) -> bool {
        x < self.width && y < self.height
    }

    pub fn get(&self, x: u16, y: u16) -> &Cell {
        &self.cells[y as usize * self.width as usize + x as usize]
    }

    pub fn put(&mut self, x: u16, y: u16, cell: Cell) {
        if self.in_bounds(x, y) {
            self.cells[y as usize * self.width as usize + x as usize] = cell;
        }
    }

    /// Write a grapheme known to be `width` columns wide at `(x, y)`.
    ///
    /// When `width == 2` the following cell becomes a continuation cell.
    pub fn put_grapheme(&mut self, x: u16, y: u16, width: usize, cell: Cell) {
        match width {
            0 | 1 => self.put(x, y, cell),
            _ => {
                self.put(x, y, cell);
                self.put(x + 1, y, Cell::blank());
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

/// Daemon → frontend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg {
    Redraw {
        grid: Grid,
        cursor: (u16, u16),
        cursor_visible: bool,
    },
    Quit,
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Encode `msg` and write it as one length-prefixed frame.
pub fn write_msg<W, M>(w: &mut W, msg: &M) -> Result<()>
where
    W: Write,
    M: Serialize,
{
    let payload = rmp_serde::to_vec_named(msg)?;
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

pub fn write_client_msg<W: Write>(w: &mut W, msg: &ClientMsg) -> Result<()> {
    write_msg(w, msg)
}

pub fn write_server_msg<W: Write>(w: &mut W, msg: &ServerMsg) -> Result<()> {
    write_msg(w, msg)
}

pub fn read_client_msg<R: Read>(r: &mut R) -> Result<Option<ClientMsg>> {
    read_msg(r)
}

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
            },
            ServerMsg::Quit,
        ]
    }

    #[test]
    fn client_messages_round_trip_named() {
        for msg in client_samples() {
            let mut buf = Vec::new();
            write_msg(&mut buf, &msg).unwrap();
            let decoded: ClientMsg = read_msg(&mut Cursor::new(&buf)).unwrap().unwrap();
            assert_eq!(msg, decoded);
        }
    }

    #[test]
    fn server_messages_round_trip_named() {
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
