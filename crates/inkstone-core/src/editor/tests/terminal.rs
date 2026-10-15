//! `:term`: a program that draws, and the buffer that shows what it drew.
//!
//! The process itself belongs to the session — `crates/inkstone-server` is where
//! a pty is really opened — so what is tested here is the kernel's half: what
//! `:term` makes, where the keys go, and what the screen does with what comes
//! back.

use super::*;
use crate::buffer::Buffer;

fn terminal(program: &str) -> Editor {
    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("term {program}"));
    ed
}

#[test]
fn a_terminal_is_a_buffer_with_a_screen_and_a_title() {
    let ed = terminal("myprog");

    assert!(ed.buffer.is_terminal());
    assert_eq!(ed.buffer.display_name(), "!myprog");
    assert_eq!(ed.buffer.path(), None, "a program's screen is not a file");
    assert!(ed.terminal_running());
    // The screen is the size of the window, less the rows of chrome.
    let (cols, rows) = ed.terminal_size();
    assert_eq!(cols, ed.viewport.width as usize);
    assert_eq!(rows, ed.viewport.height as usize - 2);
    let screen = ed.buffer.terminal().expect("a screen");
    assert_eq!((screen.cols(), screen.rows()), (cols, rows));
}

#[test]
fn the_keys_are_the_programs_from_the_first_moment() {
    let mut ed = terminal("myprog");
    assert_eq!(ed.mode, Mode::Insert, "a terminal is for typing at");

    feed(&mut ed, "ab");
    assert_eq!(ed.take_terminal_input().as_deref(), Some(b"ab".as_slice()));

    // The keys a terminal has always sent: CR, escape, the delete character, and
    // the escape sequences for the arrows.
    for (notation, expected) in [
        ("<CR>", b"\r".to_vec()),
        ("<Esc>", vec![0x1b]),
        ("<BS>", vec![0x7f]),
        ("<C-c>", vec![0x03]),
        ("<Up>", b"\x1b[A".to_vec()),
        ("<Del>", b"\x1b[3~".to_vec()),
    ] {
        feed(&mut ed, notation);
        assert_eq!(
            ed.take_terminal_input().as_deref(),
            Some(expected.as_slice()),
            "{notation}"
        );
    }
}

#[test]
fn the_escape_from_a_terminal_is_c_backslash() {
    let mut ed = terminal("myprog");
    feed(&mut ed, "ab");
    let _ = ed.take_terminal_input();

    // `<C-\>` is the editor's again: the same escape vim's terminal mode has,
    // because a program that draws needs every other key — `<Esc>` included.
    feed(&mut ed, "<C-\\>");
    assert_eq!(ed.mode, Mode::Normal);
    assert!(ed.take_terminal_input().is_none(), "and it sent nothing");

    // And `i` hands the keyboard back.
    feed(&mut ed, "i");
    assert_eq!(ed.mode, Mode::Insert);
    feed(&mut ed, "x");
    assert_eq!(ed.take_terminal_input().as_deref(), Some(b"x".as_slice()));
}

#[test]
fn what_the_program_writes_becomes_the_text() {
    let mut ed = terminal("myprog");
    ed.terminal_output(b"hello\r\nworld");

    assert_eq!(ed.buffer.line_string(0).trim_end(), "hello");
    assert_eq!(ed.buffer.line_string(1).trim_end(), "world");
    assert!(!ed.is_dirty(), "a program drawing is not an edit of yours");

    // Colours come through as the cells they were painted with. The cursor is
    // after `world`, so `red` lands on the rest of that row.
    ed.terminal_output(b"\x1b[31mred");
    let screen = ed.buffer.terminal().unwrap();
    assert_eq!(screen.cell(5, 1).attr.fg, inkstone_rpc::Color::Indexed(1));
    assert_eq!(screen.cell(5, 1).ch, 'r');
}

#[test]
fn a_terminal_is_not_written_anywhere() {
    let mut ed = terminal("myprog");
    ed.execute_cmdline("w");
    assert_eq!(
        ed.message, "E: a terminal is a program's screen, not a file",
        "a save has nowhere to put a screen"
    );
}

#[test]
fn kill_asks_the_session_to_stop_the_program() {
    let mut ed = terminal("myprog");
    assert!(!ed.take_terminal_kill());
    ed.execute_cmdline("kill");
    assert!(ed.take_terminal_kill(), "the session is told");
    assert!(!ed.take_terminal_kill(), "and told once");
}

#[test]
fn a_program_that_has_exited_says_so_and_gives_the_keys_back() {
    let mut ed = terminal("myprog");
    ed.terminal_output(b"bye\r\n");
    ed.terminal_exited(Some(0));

    assert!(!ed.terminal_running());
    assert_eq!(
        ed.mode,
        Mode::Normal,
        "a dead program has nothing to type at"
    );
    assert_eq!(ed.message, "[process exited 0]");
    assert!(
        ed.buffer.text().contains("[process exited 0]"),
        "and it is in the scrollback: {}",
        ed.buffer.text()
    );
}

#[test]
fn the_size_the_program_is_told_changes_only_when_it_does() {
    let mut ed = terminal("myprog");
    assert!(ed.take_terminal_resize().is_none(), "nothing to say yet");

    ed.handle_resize(40, 12);
    assert_eq!(ed.take_terminal_resize(), Some((40, 10)), "less the chrome");
    assert!(ed.take_terminal_resize().is_none(), "and said once");
}

#[test]
fn a_second_terminal_is_refused_while_one_is_running() {
    let mut ed = terminal("myprog");
    ed.execute_cmdline("term other");
    assert_eq!(
        ed.message,
        "E: a terminal is already running (:kill stops it)"
    );
}

#[test]
fn a_terminal_keeps_its_screen_when_the_window_splits() {
    // The screen belongs to the buffer, not to the window that shows it: a
    // split is a second look at the same screen.
    let mut ed = terminal("myprog");
    ed.terminal_output(b"drawn");
    let before = ed.buffer.text();
    ed.execute_cmdline("sp");
    assert_eq!(ed.buffer.text(), before);
    assert!(ed.buffer.is_terminal());
}

#[test]
fn the_status_line_says_which_of_the_two_the_keys_are_for() {
    let ed = terminal("myprog");
    assert!(ed.status().contains("TERMINAL"));
    let _ = Buffer::empty();
}
