//! `inkstone` — the terminal frontend.
//!
//! The frontend is deliberately dumb: it translates terminal events into
//! [`ClientMsg`]s, and blits the rows the daemon sends. It holds no editor
//! state at all, which is what lets a windowed frontend draw the same screen.
//! Everything below the drawing — finding and starting the daemon, framing,
//! folding frames into a `Screen` — is [`inkstone_client`].

use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;

use anyhow::{Context, Result};
use clap::Parser;
use crossbeam_channel::{unbounded, Receiver, Sender};
use crossterm::event::{self, Event, KeyEventKind};
#[cfg(unix)]
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::style::{Attribute, Color as CtColor, Colors, Print, SetAttribute, SetColors};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute, queue, terminal};
use inkstone_client::Daemon;

use inkstone_rpc::{
    ClientMsg, Color, KeyCode, KeyMsg, Mod, Param, PointerKind, PointerMsg, Screen, ServerMsg,
    ATTR_BOLD, ATTR_FAINT, ATTR_ITALIC, ATTR_REVERSE, ATTR_STRIKETHROUGH, ATTR_UNDERLINE,
};

#[derive(Debug, Parser)]
#[command(
    name = "inkstone",
    version,
    about = "A modal terminal editor: crossterm frontend over a headless daemon"
)]
struct Args {
    /// File to open. Created on write if it does not exist.
    file: Option<PathBuf>,

    /// The daemon to talk to. Defaults to `inkstone-server` next to this executable.
    #[arg(long, value_name = "PATH")]
    server: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut daemon = Daemon::spawn(args.server)?;

    let input_tx = spawn_input_reader();

    install_panic_hook();

    let result = {
        let _guard = TerminalGuard::new()?;
        run(&mut daemon, input_tx, args.file)
    };

    // Restore the terminal before waiting on the daemon, so a hung child
    // cannot leave the user in raw mode.
    let _ = daemon.wait();
    result
}

/// The main event loop. Returns when the daemon or the terminal goes away.
fn run(daemon: &mut Daemon, input_rx: Receiver<ClientMsg>, file: Option<PathBuf>) -> Result<()> {
    let (cols, rows) = terminal::size().context("failed to read the terminal size")?;
    daemon.send(&ClientMsg::Resize { cols, rows })?;
    if let Some(path) = file {
        daemon.send(&ClientMsg::Command {
            name: "edit_file".to_string(),
            params: vec![Param::Str(path.to_string_lossy().into_owned())],
        })?;
    }

    // The screen as the daemon has drawn it: a full frame replaces it and every
    // frame after that carries the rows that changed, so this is neither editor
    // state nor a guess — it is what is on the glass.
    let mut screen = Screen::new();
    let frames = daemon.frames().clone();
    loop {
        crossbeam_channel::select! {
            recv(frames) -> message => match message {
                Ok(ServerMsg::Redraw { grid, cursor, cursor_visible, insert }) => {
                    let rows = screen.redraw(grid, cursor, cursor_visible, insert);
                    draw(&mut io::stdout().lock(), &screen, &rows)?;
                }
                Ok(ServerMsg::Rows { updates, cursor, cursor_visible, insert }) => {
                    let rows = screen.rows(&updates, cursor, cursor_visible, insert);
                    draw(&mut io::stdout().lock(), &screen, &rows)?;
                }
                Ok(ServerMsg::Clipboard(text)) => {
                    // A terminal's clipboard is the terminal's: ask for it and
                    // carry on.
                    let mut out = io::stdout().lock();
                    out.write_all(&osc52(&text))?;
                    out.flush()?;
                }
                Ok(ServerMsg::Quit) => {
                    let _ = daemon.send(&ClientMsg::Shutdown);
                    break;
                }
                // The daemon hung up: nothing left to draw, so leave cleanly.
                Err(_) => break,
            },
            recv(input_rx) -> message => match message {
                Ok(message) => daemon.send(&message)?,
                Err(_) => break,
            },
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

/// Read terminal events on a helper thread and forward them as client messages.
fn spawn_input_reader() -> Receiver<ClientMsg> {
    let (tx, rx) = unbounded();
    thread::Builder::new()
        .name("inkstone-input-reader".to_string())
        .spawn(move || input_loop(tx))
        .expect("failed to spawn the input thread");
    rx
}

fn input_loop(tx: Sender<ClientMsg>) {
    loop {
        match event::read() {
            Ok(event) => {
                if let Some(msg) = to_client_msg(event) {
                    if tx.send(msg).is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                eprintln!("edit: terminal input error: {e}");
                // Closing the channel ends the session.
                return;
            }
        }
    }
}

/// What one terminal event means to the daemon, if anything.
///
/// Focus and mouse events are not part of milestone 1, and a key the protocol
/// cannot express is dropped rather than guessed at.
/// Ask the terminal to put `text` on the system clipboard: OSC 52.
///
/// A terminal has no clipboard API of its own, and this is the escape sequence
/// that asks the terminal's — `ESC ] 52 ; c ; <base64> BEL`. Terminals that do
/// not know it ignore it, which is why it is sent and forgotten rather than
/// waited on.
fn osc52(text: &str) -> Vec<u8> {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes())).into_bytes()
}

/// The standard base64 alphabet, for the one place that needs it.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for shift in [18, 12, 6, 0] {
            let index = ((n >> shift) & 0x3f) as usize;
            out.push(ALPHABET[index] as char);
        }
        if chunk.len() < 3 {
            out.pop();
            if chunk.len() < 2 {
                out.pop();
            }
            out.push('=');
            if chunk.len() < 2 {
                out.push('=');
            }
        }
    }
    out
}

fn to_client_msg(event: Event) -> Option<ClientMsg> {
    match event {
        Event::Key(key) => {
            if key.kind == KeyEventKind::Release {
                return None;
            }
            to_key_msg(key).map(ClientMsg::Input)
        }
        Event::Resize(cols, rows) => Some(ClientMsg::Resize { cols, rows }),
        // The pointer, in screen cells: what a cell means is the daemon's
        // business, so nothing is decided here.
        Event::Mouse(mouse) => to_pointer_msg(mouse).map(ClientMsg::Pointer),
        // A paste is text, not keys: sending it as keys would run whatever
        // commands it happens to spell.
        Event::Paste(text) => Some(ClientMsg::Text(text)),
        _ => None,
    }
}

/// The protocol's pointer event for a terminal mouse event, if it is one the
/// protocol carries.
///
/// The middle and right buttons, and a bare move with nothing pressed, are not
/// part of it: a frontend sends what it can, and the daemon ignores what it does
/// not understand.
fn to_pointer_msg(mouse: event::MouseEvent) -> Option<PointerMsg> {
    let kind = match mouse.kind {
        event::MouseEventKind::Down(event::MouseButton::Left) => PointerKind::Press,
        event::MouseEventKind::Up(event::MouseButton::Left) => PointerKind::Release,
        event::MouseEventKind::Drag(event::MouseButton::Left) => PointerKind::Drag,
        event::MouseEventKind::ScrollUp => PointerKind::ScrollUp,
        event::MouseEventKind::ScrollDown => PointerKind::ScrollDown,
        _ => return None,
    };
    Some(PointerMsg::new(kind, mouse.column, mouse.row))
}

fn to_key_msg(key: event::KeyEvent) -> Option<KeyMsg> {
    // `BackTab` is crossterm's Shift+Tab; the protocol spells that `<S-Tab>`,
    // and dropping it would make one of its keys unreachable.
    let (code, implies_shift) = match key.code {
        event::KeyCode::BackTab => (KeyCode::Tab, true),
        other => (to_key_code(other)?, false),
    };
    let mut mods = Vec::new();
    if key.modifiers.contains(event::KeyModifiers::CONTROL) {
        mods.push(Mod::Ctrl);
    }
    if key.modifiers.contains(event::KeyModifiers::ALT) {
        mods.push(Mod::Alt);
    }
    // Shift is already baked into a character; keeping it would make `A` and
    // `<S-a>` two different keys.
    if (key.modifiers.contains(event::KeyModifiers::SHIFT) || implies_shift)
        && !matches!(code, KeyCode::Char(_))
        && !mods.contains(&Mod::Shift)
    {
        mods.push(Mod::Shift);
    }
    Some(KeyMsg { code, mods })
}

/// The protocol key for a terminal key, if the protocol has one at all.
fn to_key_code(code: event::KeyCode) -> Option<KeyCode> {
    Some(match code {
        // A tab that arrives as the character rather than as the code: some
        // terminals hand 0x09 over as a `\t` and some as a Tab, and a person
        // pressing the key means the same thing either way.
        event::KeyCode::Char('\t') => KeyCode::Tab,
        event::KeyCode::Char(c) => KeyCode::Char(c),
        event::KeyCode::Enter => KeyCode::Enter,
        event::KeyCode::Esc => KeyCode::Esc,
        event::KeyCode::Backspace => KeyCode::Backspace,
        event::KeyCode::Tab => KeyCode::Tab,
        event::KeyCode::Left => KeyCode::Left,
        event::KeyCode::Right => KeyCode::Right,
        event::KeyCode::Up => KeyCode::Up,
        event::KeyCode::Down => KeyCode::Down,
        event::KeyCode::Home => KeyCode::Home,
        event::KeyCode::End => KeyCode::End,
        event::KeyCode::Delete => KeyCode::Delete,
        event::KeyCode::PageUp => KeyCode::PageUp,
        event::KeyCode::PageDown => KeyCode::PageDown,
        // BackTab is handled by the caller; everything else (function keys,
        // Insert, ...) is not a key the protocol can express.
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Blit the screen to the terminal: the rows in `rows`, then put the cursor.
///
/// The grid is always fully populated (empty screen space is a `" "` cell), so
/// drawing is a straight write with no clearing: a cell whose text is empty is
/// the second half of a wide grapheme and is skipped. Only the rows that
/// changed are written, which is what makes a keystroke cost a line rather than
/// a screen.
fn draw(out: &mut impl Write, screen: &Screen, rows: &[u16]) -> io::Result<()> {
    let grid = screen.grid();
    let mut style = None;

    for &y in rows {
        queue!(out, cursor::MoveTo(0, y))?;
        for x in 0..grid.width {
            let cell = grid.get(x, y);
            if cell.is_continuation() {
                continue;
            }
            let wanted = (cell.fg, cell.bg, cell.attrs);
            if style != Some(wanted) {
                queue!(out, SetAttribute(Attribute::Reset))?;
                queue_attributes(out, cell.attrs)?;
                queue!(
                    out,
                    SetColors(Colors::new(to_ct_color(cell.fg), to_ct_color(cell.bg)))
                )?;
                style = Some(wanted);
            }
            queue!(out, Print(&cell.text))?;
        }
    }

    queue!(out, SetAttribute(Attribute::Reset))?;
    if screen.cursor_visible() {
        queue!(
            out,
            cursor::MoveTo(screen.cursor().0, screen.cursor().1),
            cursor::Show
        )?;
    } else {
        queue!(out, cursor::Hide)?;
    }
    out.flush()
}

fn queue_attributes(out: &mut impl Write, attrs: u16) -> io::Result<()> {
    if attrs & ATTR_BOLD != 0 {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    if attrs & ATTR_UNDERLINE != 0 {
        queue!(out, SetAttribute(Attribute::Underlined))?;
    }
    if attrs & ATTR_REVERSE != 0 {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    if attrs & ATTR_ITALIC != 0 {
        queue!(out, SetAttribute(Attribute::Italic))?;
    }
    if attrs & ATTR_FAINT != 0 {
        queue!(out, SetAttribute(Attribute::Dim))?;
    }
    if attrs & ATTR_STRIKETHROUGH != 0 {
        queue!(out, SetAttribute(Attribute::CrossedOut))?;
    }
    Ok(())
}

fn to_ct_color(color: Color) -> CtColor {
    match color {
        Color::Reset => CtColor::Reset,
        Color::Indexed(i) => CtColor::AnsiValue(i),
        Color::Rgb(r, g, b) => CtColor::Rgb { r, g, b },
    }
}

// ---------------------------------------------------------------------------
// Terminal lifecycle
// ---------------------------------------------------------------------------

/// Puts the terminal into raw mode and the alternate screen, and — crucially —
/// always puts it back.
struct TerminalGuard;

impl TerminalGuard {
    fn new() -> Result<Self> {
        terminal::enable_raw_mode().context("failed to enable raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen)
            .context("failed to enter the alternate screen")?;
        // The pointer, so a click can put the cursor where it pointed. A user
        // who wants the terminal's own selection holds Shift, which is what
        // every terminal application asks of them.
        let _ = execute!(io::stdout(), event::EnableMouseCapture);
        // Bracketed paste: the terminal then says "this was pasted", which
        // becomes one `Text` message instead of a burst of keys — a paste of
        // `dd` deletes a line, and one of `:q!` quits, otherwise.
        //
        // Only where crossterm can hear it: `Event::Paste` is parsed by its
        // Unix backend (`event/sys/unix/parse.rs`) and has no Windows
        // counterpart, so on Windows the markers would be characters the
        // terminal decided to keep.
        #[cfg(unix)]
        let _ = execute!(io::stdout(), EnableBracketedPaste);
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), event::DisableMouseCapture);
        #[cfg(unix)]
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

/// Restore the terminal before a panic message is printed over it.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use inkstone_rpc::{Cell, Grid};

    /// Render `grid` the way a full frame is drawn: every row, then the cursor.
    fn render(grid: &Grid, cursor: (u16, u16), visible: bool) -> String {
        let mut screen = Screen::new();
        let mut out = Vec::new();
        let rows = screen.redraw(grid.clone(), cursor, visible, false);
        draw(&mut out, &screen, &rows).unwrap();
        String::from_utf8(out).expect("terminal output is valid utf-8")
    }

    #[test]
    fn draw_writes_only_the_rows_it_is_given() {
        let mut grid = Grid::new(3, 3);
        for y in 0..3 {
            grid.put(0, y, Cell::new("x"));
        }
        let mut screen = Screen::new();
        screen.redraw(grid, (0, 0), true, false);

        let mut out = Vec::new();
        draw(&mut out, &screen, &[1]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("\x1b[2;1H"),
            "the row is drawn where it belongs: {text:?}"
        );
        assert_eq!(
            text.matches('x').count(),
            1,
            "and nothing else is written: {text:?}"
        );
    }

    #[test]
    fn draw_writes_every_cell_once_and_skips_wide_continuations() {
        let mut grid = Grid::new(4, 1);
        grid.put_grapheme(0, 0, 2, Cell::new("好"));
        grid.put(2, 0, Cell::new("b"));
        let text = render(&grid, (2, 0), true);

        assert!(
            text.contains("好b"),
            "the glyphs are printed in order: {text:?}"
        );
        assert_eq!(
            text.matches(' ').count(),
            1,
            "only the blank cell is a space"
        );
        assert!(text.contains("\x1b[1;1H"), "the cursor is homed: {text:?}");
    }

    #[test]
    fn draw_only_emits_attributes_when_the_style_changes() {
        let mut grid = Grid::new(3, 1);
        for x in 0..3 {
            grid.put(x, 0, Cell::new("x"));
        }
        let text = render(&grid, (0, 0), true);
        assert_eq!(
            text.matches("\x1b[0m").count(),
            2,
            "one reset for the row, one before the cursor: {text:?}"
        );
    }

    #[test]
    fn draw_marks_reverse_video_cells() {
        let mut grid = Grid::new(2, 2);
        for x in 0..2 {
            grid.put(x, 1, Cell::new(" ").with_attrs(ATTR_REVERSE));
        }
        let text = render(&grid, (0, 0), true);
        assert!(
            text.contains("\x1b[7m"),
            "reverse video for the status row: {text:?}"
        );
    }

    #[test]
    fn draw_shows_or_hides_the_cursor() {
        let grid = Grid::new(2, 1);
        assert!(render(&grid, (0, 0), true).contains("\x1b[?25h"));
        assert!(render(&grid, (0, 0), false).contains("\x1b[?25l"));
    }

    fn key(code: event::KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn shifted_characters_do_not_carry_a_shift_modifier() {
        let msg = to_key_msg(key(event::KeyCode::Char('A'), KeyModifiers::SHIFT)).unwrap();
        assert_eq!(msg, KeyMsg::new(KeyCode::Char('A')));
    }

    #[test]
    fn control_and_alt_are_forwarded() {
        let msg = to_key_msg(key(event::KeyCode::Char('w'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(msg, KeyMsg::ctrl('w'));
        let msg = to_key_msg(key(event::KeyCode::Left, KeyModifiers::ALT)).unwrap();
        assert_eq!(
            msg,
            KeyMsg {
                code: KeyCode::Left,
                mods: vec![Mod::Alt]
            }
        );
    }

    #[test]
    fn a_tab_is_the_tab_key_however_the_terminal_spells_it() {
        // Some terminals hand 0x09 over as the Tab key and some as the
        // character, and both mean the key: `<C-i>` and `<Tab>` are one byte.
        let as_code = to_key_msg(key(event::KeyCode::Tab, KeyModifiers::NONE)).unwrap();
        let as_char = to_key_msg(key(event::KeyCode::Char('\t'), KeyModifiers::NONE)).unwrap();
        assert_eq!(as_code.code, KeyCode::Tab);
        assert_eq!(as_char.code, KeyCode::Tab);
    }

    #[test]
    fn shift_is_kept_for_named_keys() {
        let msg = to_key_msg(key(event::KeyCode::Tab, KeyModifiers::SHIFT)).unwrap();
        assert_eq!(msg.mods, vec![Mod::Shift]);
    }

    /// Iterating the protocol's key list means a new `KeyCode` cannot be added
    /// without deciding how a terminal produces it.
    #[test]
    fn every_protocol_key_has_a_terminal_mapping() {
        for &code in KeyCode::ALL {
            let terminal_code = match code {
                KeyCode::Char(c) => event::KeyCode::Char(c),
                KeyCode::Enter => event::KeyCode::Enter,
                KeyCode::Esc => event::KeyCode::Esc,
                KeyCode::Backspace => event::KeyCode::Backspace,
                KeyCode::Tab => event::KeyCode::Tab,
                KeyCode::Left => event::KeyCode::Left,
                KeyCode::Right => event::KeyCode::Right,
                KeyCode::Up => event::KeyCode::Up,
                KeyCode::Down => event::KeyCode::Down,
                KeyCode::Home => event::KeyCode::Home,
                KeyCode::End => event::KeyCode::End,
                KeyCode::Delete => event::KeyCode::Delete,
                KeyCode::PageUp => event::KeyCode::PageUp,
                KeyCode::PageDown => event::KeyCode::PageDown,
            };
            let decoded = to_key_msg(key(terminal_code, KeyModifiers::empty()));
            assert_eq!(
                decoded,
                Some(KeyMsg::new(code)),
                "{code:?} cannot be produced by the frontend"
            );
        }
    }

    /// Same idea for cell attributes: a new bit has to be translated or the
    /// style is silently lost.
    #[test]
    fn every_attribute_bit_reaches_the_terminal() {
        let mut rendered = Vec::new();
        for &bit in inkstone_rpc::ATTRS {
            let mut out = Vec::new();
            queue_attributes(&mut out, bit).unwrap();
            assert!(!out.is_empty(), "attribute bit {bit} is not translated");
            rendered.push(out);
        }
        for (i, first) in rendered.iter().enumerate() {
            for (j, second) in rendered.iter().enumerate() {
                if i != j {
                    assert_ne!(first, second, "two attribute bits render identically");
                }
            }
        }

        let all = inkstone_rpc::ATTRS.iter().fold(0u16, |acc, bit| acc | bit);
        let mut out = Vec::new();
        queue_attributes(&mut out, all).unwrap();
        for bit in &rendered {
            let needle = String::from_utf8_lossy(bit);
            assert!(
                String::from_utf8_lossy(&out).contains(needle.as_ref()),
                "a cell with every attribute set is missing {needle:?}"
            );
        }
    }

    #[test]
    fn shift_tab_is_reported_as_a_shifted_tab() {
        let msg = to_key_msg(key(event::KeyCode::BackTab, KeyModifiers::SHIFT)).unwrap();
        assert_eq!(
            msg,
            KeyMsg {
                code: KeyCode::Tab,
                mods: vec![Mod::Shift]
            }
        );
        // Even when the terminal forgets to say so.
        let msg = to_key_msg(key(event::KeyCode::BackTab, KeyModifiers::empty())).unwrap();
        assert_eq!(msg.mods, vec![Mod::Shift]);
    }

    #[test]
    fn unmapped_terminal_keys_are_dropped() {
        assert!(to_key_msg(key(event::KeyCode::F(5), KeyModifiers::empty())).is_none());
        assert!(to_key_msg(key(event::KeyCode::Insert, KeyModifiers::empty())).is_none());
    }

    #[test]
    fn a_paste_becomes_text_and_a_key_becomes_a_key() {
        assert_eq!(
            to_client_msg(Event::Paste("dd\nsecond".to_string())),
            Some(ClientMsg::Text("dd\nsecond".to_string())),
            "a paste is text: it must not be run as commands"
        );
        assert_eq!(
            to_client_msg(event::Event::Resize(120, 40)),
            Some(ClientMsg::Resize {
                cols: 120,
                rows: 40
            })
        );
        assert_eq!(
            to_client_msg(Event::Key(KeyEvent::new(
                event::KeyCode::Char('x'),
                KeyModifiers::empty()
            ))),
            Some(ClientMsg::Input(KeyMsg::new(KeyCode::Char('x'))))
        );
        // A key that is only released is not a key press, and a key the
        // protocol cannot express is dropped rather than guessed at.
        assert_eq!(
            to_client_msg(Event::Key(KeyEvent::new_with_kind(
                event::KeyCode::Char('x'),
                KeyModifiers::empty(),
                KeyEventKind::Release,
            ))),
            None
        );
        assert_eq!(
            to_client_msg(Event::Key(KeyEvent::new(
                event::KeyCode::F(5),
                KeyModifiers::empty()
            ))),
            None
        );
        assert_eq!(to_client_msg(Event::FocusGained), None);
    }

    #[test]
    fn base64_is_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // The bytes of a CJK character, which is what a real copy looks like.
        assert_eq!(base64("你".as_bytes()), "5L2g");
    }

    #[test]
    fn a_copy_asks_the_terminal_for_its_clipboard() {
        assert_eq!(
            String::from_utf8(osc52("hi")).unwrap(),
            "\u{1b}]52;c;aGk=\u{7}",
            "OSC 52, with the text in base64"
        );
    }

    #[test]
    fn a_mouse_event_becomes_a_pointer_event() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

        let mouse = |kind| MouseEvent {
            kind,
            column: 3,
            row: 4,
            modifiers: KeyModifiers::empty(),
        };
        assert_eq!(
            to_pointer_msg(mouse(MouseEventKind::Down(MouseButton::Left))),
            Some(PointerMsg::new(PointerKind::Press, 3, 4))
        );
        assert_eq!(
            to_pointer_msg(mouse(MouseEventKind::Up(MouseButton::Left))),
            Some(PointerMsg::new(PointerKind::Release, 3, 4))
        );
        assert_eq!(
            to_pointer_msg(mouse(MouseEventKind::Drag(MouseButton::Left))),
            Some(PointerMsg::new(PointerKind::Drag, 3, 4))
        );
        assert_eq!(
            to_pointer_msg(mouse(MouseEventKind::ScrollDown)),
            Some(PointerMsg::new(PointerKind::ScrollDown, 3, 4))
        );

        // Buttons the protocol does not carry, and a move with nothing pressed.
        assert_eq!(
            to_pointer_msg(mouse(MouseEventKind::Down(MouseButton::Middle))),
            None
        );
        assert_eq!(to_pointer_msg(mouse(MouseEventKind::Moved)), None);
    }

    #[test]
    fn colors_map_across() {
        assert_eq!(to_ct_color(Color::Reset), CtColor::Reset);
        assert_eq!(to_ct_color(Color::Indexed(7)), CtColor::AnsiValue(7));
        assert_eq!(
            to_ct_color(Color::Rgb(1, 2, 3)),
            CtColor::Rgb { r: 1, g: 2, b: 3 }
        );
    }
}
