//! `edit` — the terminal frontend.
//!
//! The frontend is deliberately dumb: it translates terminal events into
//! [`ClientMsg`]s, and blits whatever [`ServerMsg`] grid comes back. It holds
//! no editor state at all, so a future GUI or a remote client can reuse the
//! daemon unchanged.

use std::io::{self, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result};
use clap::Parser;
use crossbeam_channel::{unbounded, Receiver, Sender};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::style::{Attribute, Color as CtColor, Colors, Print, SetAttribute, SetColors};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute, queue, terminal};

use edit_rpc::{
    read_server_msg, write_client_msg, ClientMsg, Color, Grid, KeyCode, KeyMsg, Mod, Param,
    ServerMsg, ATTR_BOLD, ATTR_REVERSE, ATTR_UNDERLINE,
};

#[derive(Debug, Parser)]
#[command(
    name = "edit",
    version,
    about = "A modal terminal editor: crossterm frontend over a headless daemon"
)]
struct Args {
    /// File to open. Created on write if it does not exist.
    file: Option<PathBuf>,

    /// The daemon to talk to. Defaults to `edit-server` next to this executable.
    #[arg(long, value_name = "PATH")]
    server: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let server = resolve_server(args.server)?;

    let mut child = Command::new(&server)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| {
            let looked_next_to = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.display().to_string()))
                .unwrap_or_else(|| "?".to_string());
            format!(
                "failed to start the daemon at {}\n\
                 looked for it next to:   {}\n\
                 build it first:          cargo build --workspace\n\
                 or point at it:          edit --server <PATH>",
                server.display(),
                looked_next_to,
            )
        })?;

    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");

    let server_rx = spawn_server_reader(stdout);
    let input_tx = spawn_input_reader();

    install_panic_hook();

    let result = {
        let _guard = TerminalGuard::new()?;
        run(stdin, server_rx, input_tx, args.file)
    };

    // Restore the terminal before waiting on the daemon, so a hung child
    // cannot leave the user in raw mode.
    drop(child.stdin.take());
    let _ = child.wait();
    result
}

/// The main event loop. Returns when the daemon or the terminal goes away.
fn run(
    mut stdin: std::process::ChildStdin,
    server_rx: Receiver<ServerMsg>,
    input_rx: Receiver<ClientMsg>,
    file: Option<PathBuf>,
) -> Result<()> {
    let (cols, rows) = terminal::size().context("failed to read the terminal size")?;
    send(&mut stdin, &ClientMsg::Resize { cols, rows })?;
    if let Some(path) = file {
        send(
            &mut stdin,
            &ClientMsg::Command {
                name: "edit_file".to_string(),
                params: vec![Param::Str(path.to_string_lossy().into_owned())],
            },
        )?;
    }

    loop {
        crossbeam_channel::select! {
            recv(server_rx) -> message => match message {
                Ok(ServerMsg::Redraw { grid, cursor, cursor_visible }) => {
                    draw(&mut io::stdout().lock(), &grid, cursor, cursor_visible)?;
                }
                Ok(ServerMsg::Quit) => {
                    let _ = send(&mut stdin, &ClientMsg::Shutdown);
                    break;
                }
                // The daemon hung up: nothing left to draw, so leave cleanly.
                Err(_) => break,
            },
            recv(input_rx) -> message => match message {
                Ok(message) => send(&mut stdin, &message)?,
                Err(_) => break,
            },
        }
    }
    Ok(())
}

fn send(stdin: &mut impl Write, msg: &ClientMsg) -> Result<()> {
    write_client_msg(stdin, msg).context("failed to send a message to the daemon")?;
    stdin.flush().context("failed to flush the daemon pipe")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

/// Decode daemon frames on a helper thread so the main loop can select.
fn spawn_server_reader(stdout: std::process::ChildStdout) -> Receiver<ServerMsg> {
    let (tx, rx) = unbounded();
    thread::Builder::new()
        .name("edit-rpc-reader".to_string())
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

/// Read terminal events on a helper thread and forward them as client messages.
fn spawn_input_reader() -> Receiver<ClientMsg> {
    let (tx, rx) = unbounded();
    thread::Builder::new()
        .name("edit-input-reader".to_string())
        .spawn(move || input_loop(tx))
        .expect("failed to spawn the input thread");
    rx
}

fn input_loop(tx: Sender<ClientMsg>) {
    loop {
        match event::read() {
            Ok(Event::Key(key)) => {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                if let Some(key) = to_key_msg(key) {
                    if tx.send(ClientMsg::Input(key)).is_err() {
                        return;
                    }
                }
            }
            Ok(Event::Resize(cols, rows)) => {
                if tx.send(ClientMsg::Resize { cols, rows }).is_err() {
                    return;
                }
            }
            // Focus and mouse events are not part of milestone 1.
            Ok(_) => {}
            Err(e) => {
                eprintln!("edit: terminal input error: {e}");
                // Closing the channel ends the session.
                return;
            }
        }
    }
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

/// Blit a grid to the terminal.
///
/// The grid is always fully populated (empty screen space is a `" "` cell), so
/// drawing is a straight write with no clearing: a cell whose text is empty is
/// the second half of a wide grapheme and is skipped.
fn draw(
    out: &mut impl Write,
    grid: &Grid,
    cursor: (u16, u16),
    cursor_visible: bool,
) -> io::Result<()> {
    let mut style = None;

    for y in 0..grid.height {
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
    if cursor_visible {
        queue!(out, cursor::MoveTo(cursor.0, cursor.1), cursor::Show)?;
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
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
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

/// Find the daemon: an explicit path, else a sibling of this executable, else
/// whatever is on `PATH`.
fn resolve_server(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    let name = if cfg!(windows) {
        "edit-server.exe"
    } else {
        "edit-server"
    };
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)));
    match sibling {
        Some(path) if path.exists() => Ok(path),
        // Not built (or moved): fall back to whatever is on `PATH`.
        _ => Ok(PathBuf::from(name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use edit_rpc::Cell;

    fn render(grid: &Grid, cursor: (u16, u16), visible: bool) -> String {
        let mut out = Vec::new();
        draw(&mut out, grid, cursor, visible).unwrap();
        String::from_utf8(out).expect("terminal output is valid utf-8")
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
        for &bit in edit_rpc::ATTRS {
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

        let all = edit_rpc::ATTRS.iter().fold(0u16, |acc, bit| acc | bit);
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
    fn colors_map_across() {
        assert_eq!(to_ct_color(Color::Reset), CtColor::Reset);
        assert_eq!(to_ct_color(Color::Indexed(7)), CtColor::AnsiValue(7));
        assert_eq!(
            to_ct_color(Color::Rgb(1, 2, 3)),
            CtColor::Rgb { r: 1, g: 2, b: 3 }
        );
    }

    #[test]
    fn the_server_defaults_to_a_sibling_binary() {
        let explicit = PathBuf::from("/tmp/edit-server");
        assert_eq!(resolve_server(Some(explicit.clone())).unwrap(), explicit);
        // With no explicit path we either find a sibling or fall back to PATH;
        // either way the name is what we asked for.
        let resolved = resolve_server(None).unwrap();
        assert!(resolved.ends_with(if cfg!(windows) {
            "edit-server.exe"
        } else {
            "edit-server"
        }));
    }
}
