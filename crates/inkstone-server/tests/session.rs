//! Integration tests for the daemon: the in-process session seam, plus one
//! end-to-end run against the real `inkstone-server` binary over its stdio pipes.
//!
//! Every test that looks at the screen folds the frames the way a frontend
//! does — a full frame replaces the screen, a delta patches it — so the delta
//! path is what these tests actually exercise.

use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use inkstone_core::Editor;
use inkstone_rpc::{
    read_server_msg, write_client_msg, ClientMsg, KeyCode, KeyMsg, Param, PointerKind, PointerMsg,
    Screen, ServerMsg, ATTR_REVERSE,
};
use inkstone_server::{Response, Session};

const TIMEOUT: Duration = Duration::from_secs(10);

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("inkstone-server-tests")
        .join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn typed(c: char) -> ClientMsg {
    ClientMsg::Input(KeyMsg::new(KeyCode::Char(c)))
}

fn pressed(code: KeyCode) -> ClientMsg {
    ClientMsg::Input(KeyMsg::new(code))
}

fn type_str(s: &str) -> Vec<ClientMsg> {
    s.chars().map(typed).collect()
}

/// Fold one frame into `screen`, the way a frontend does.
fn fold_frame(frame: &ServerMsg, screen: &mut Screen) {
    match frame {
        ServerMsg::Redraw {
            grid,
            cursor,
            cursor_visible,
            insert,
        } => {
            screen.redraw(grid.clone(), *cursor, *cursor_visible, *insert);
        }
        ServerMsg::Rows {
            updates,
            cursor,
            cursor_visible,
            insert,
        } => {
            screen.rows(updates, *cursor, *cursor_visible, *insert);
        }
        // A clipboard frame is for the frontend, not for the screen.
        ServerMsg::Clipboard(_) | ServerMsg::Quit => {}
    }
}

/// The clipboard text a response carried, if any.
fn clipboard(response: &Response) -> Option<String> {
    response.messages.iter().find_map(|msg| match msg {
        ServerMsg::Clipboard(text) => Some(text.clone()),
        _ => None,
    })
}

/// Fold everything a response carried into `screen`.
fn fold(response: &Response, screen: &mut Screen) {
    for msg in &response.messages {
        fold_frame(msg, screen);
    }
}

/// Send these messages and fold the replies, the way a frontend does.
fn feed(session: &mut Session, screen: &mut Screen, msgs: Vec<ClientMsg>) {
    for msg in msgs {
        fold(&session.handle(msg), screen);
    }
}

fn session() -> Session {
    Session::new(Editor::new(None).expect("editor"))
}

/// What the daemon sends for one client message: a full frame or a delta.
fn frames(session: &mut Session, msg: ClientMsg) -> Vec<ServerMsg> {
    session.handle(msg).messages
}

#[test]
fn resize_produces_a_frame_of_the_requested_size() {
    let mut session = session();
    let mut screen = Screen::new();
    let messages = frames(&mut session, ClientMsg::Resize { cols: 40, rows: 6 });
    assert!(
        matches!(messages[..], [ServerMsg::Redraw { .. }]),
        "a resize needs a whole screen: {messages:?}"
    );
    fold(
        &Response {
            messages,
            quit: false,
        },
        &mut screen,
    );

    let grid = screen.grid();
    assert_eq!(grid.width, 40);
    assert_eq!(grid.height, 6);
    assert!(grid.row_text(0).trim_end().is_empty());
    assert!(
        grid.row_text(4).starts_with("-- NORMAL --"),
        "status row: {:?}",
        grid.row_text(4)
    );
}

#[test]
fn typing_and_saving_round_trips_through_the_daemon() {
    let dir = temp_dir("typing");
    let path = dir.join("out.txt");
    let _ = std::fs::remove_file(&path);

    let mut session = session();
    let mut screen = Screen::new();
    // Wide enough that the `written` message is not truncated.
    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Resize { cols: 120, rows: 6 }],
    );

    let mut keys = vec![typed('i')];
    keys.extend(type_str("hello"));
    keys.push(pressed(KeyCode::Esc));
    feed(&mut session, &mut screen, keys);

    let grid = screen.grid();
    assert_eq!(grid.row_text(0).trim_end(), "hello");
    assert!(
        grid.row_text(4).contains("[+]"),
        "the buffer is dirty: {:?}",
        grid.row_text(4)
    );
    assert!(
        !grid.row_text(4).contains("INSERT"),
        "we left insert mode: {:?}",
        grid.row_text(4)
    );

    let response = session.handle(ClientMsg::Command {
        name: "save".into(),
        params: vec![Param::Str(path.to_string_lossy().into_owned())],
    });
    fold(&response, &mut screen);
    assert!(
        screen.grid().row_text(5).contains("written"),
        "message row: {:?}",
        screen.grid().row_text(5)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    assert!(!response.quit);
}

/// A keystroke that changes nothing but the cursor is a frame with no rows in
/// it, and one that changes a line carries that line and nothing else.
#[test]
fn a_frame_carries_only_the_rows_that_changed() {
    let mut session = session();
    let mut screen = Screen::new();
    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Resize { cols: 20, rows: 6 }],
    );

    let mut keys = vec![typed('i')];
    keys.extend(type_str("ab"));
    keys.push(pressed(KeyCode::Esc));
    feed(&mut session, &mut screen, keys);

    // Moving the cursor does not change the screen at 20 columns wide: the
    // status line is truncated before it gets to the cursor position.
    let messages = frames(&mut session, pressed(KeyCode::Left));
    match &messages[..] {
        [ServerMsg::Rows { updates, .. }] => assert!(
            updates.is_empty(),
            "moving the cursor changed {} rows",
            updates.len()
        ),
        other => panic!("expected one delta frame, got {other:?}"),
    }

    // Typing `x` changes the row the cursor is on: `b`, then blanks.
    let messages = frames(&mut session, typed('x'));
    match &messages[..] {
        [ServerMsg::Rows { updates, .. }] => {
            assert_eq!(updates.len(), 1, "one row: {updates:?}");
            assert_eq!(updates[0].y, 0);
            assert_eq!(updates[0].runs[0].cell.text, "b", "the row after the edit");
            assert_eq!(updates[0].runs[1].count, 19, "the rest of them blanks");
        }
        other => panic!("expected one delta frame, got {other:?}"),
    }
}

/// Text that was never a key — a paste, or what an input method committed —
/// lands at the cursor and takes one `u` to remove, whatever it spells.
#[test]
fn pasted_text_arrives_as_one_change() {
    let mut session = session();
    let mut screen = Screen::new();
    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Resize { cols: 20, rows: 6 }, typed('i')],
    );
    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Text("dd\na\nb\n".to_string())],
    );
    assert_eq!(screen.grid().row_text(0).trim_end(), "dd");
    assert_eq!(screen.grid().row_text(1).trim_end(), "a");
    assert_eq!(screen.grid().row_text(2).trim_end(), "b");

    feed(
        &mut session,
        &mut screen,
        vec![pressed(KeyCode::Esc), typed('u')],
    );
    assert_eq!(
        screen.grid().row_text(0).trim_end(),
        "",
        "one `u`, even though the paste spelled a command"
    );
}

/// A click puts the cursor where it pointed, a drag selects, and a release
/// without a drag is only a click.
#[test]
fn the_pointer_works_over_the_wire() {
    let mut session = session();
    let mut screen = Screen::new();
    feed(
        &mut session,
        &mut screen,
        vec![
            ClientMsg::Resize { cols: 20, rows: 8 },
            typed('i'),
            ClientMsg::Text("one\ntwo\nthree\n".to_string()),
            pressed(KeyCode::Esc),
            typed('g'),
            typed('g'),
        ],
    );
    assert_eq!(screen.grid().row_text(0).trim_end(), "one");

    // A press at the start of the second row: the cursor goes there, and the
    // selection it starts is visible as reverse video.
    let messages = frames(
        &mut session,
        ClientMsg::Pointer(PointerMsg::new(PointerKind::Press, 0, 1)),
    );
    fold(
        &Response {
            messages,
            quit: false,
        },
        &mut screen,
    );
    assert_eq!(screen.cursor(), (0, 1));
    assert!(
        screen.grid().get(0, 1).attrs & ATTR_REVERSE != 0,
        "the character under the cursor is selected"
    );

    // Dragging to the last character of the line extends the selection over it.
    let messages = frames(
        &mut session,
        ClientMsg::Pointer(PointerMsg::new(PointerKind::Drag, 2, 1)),
    );
    fold(
        &Response {
            messages,
            quit: false,
        },
        &mut screen,
    );
    for x in 0..3 {
        assert!(
            screen.grid().get(x, 1).attrs & ATTR_REVERSE != 0,
            "column {x} of the dragged row is selected"
        );
    }
    assert_eq!(
        screen.grid().get(3, 1).attrs & ATTR_REVERSE,
        0,
        "and nothing past it on that row"
    );

    // Copying takes the selection, and the frontend is told what it is.
    let response = session.handle(ClientMsg::Copy);
    assert_eq!(clipboard(&response), Some("two".to_string()));

    // Releasing ends the drag, and the selection stays: it is the user's.
    let messages = frames(
        &mut session,
        ClientMsg::Pointer(PointerMsg::new(PointerKind::Release, 2, 1)),
    );
    fold(
        &Response {
            messages,
            quit: false,
        },
        &mut screen,
    );
    assert!(
        screen
            .grid()
            .row_text(6)
            .trim_end()
            .starts_with("-- VISUAL --"),
        "still selecting: {:?}",
        screen.grid().row_text(6)
    );

    // A press and a release with nothing between them collapses to the cursor.
    feed(
        &mut session,
        &mut screen,
        vec![
            ClientMsg::Pointer(PointerMsg::new(PointerKind::Press, 0, 2)),
            ClientMsg::Pointer(PointerMsg::new(PointerKind::Release, 0, 2)),
        ],
    );
    assert_eq!(screen.cursor(), (0, 2));
    assert!(
        screen.grid().get(0, 2).attrs & ATTR_REVERSE == 0,
        "a click does not leave a selection behind"
    );
    assert!(
        screen
            .grid()
            .row_text(6)
            .trim_end()
            .starts_with("-- NORMAL --"),
        "and it is not visual mode any more: {:?}",
        screen.grid().row_text(6)
    );
}

/// The wheel moves the view, not the cursor.
#[test]
fn the_wheel_scrolls_without_moving_the_cursor() {
    let mut session = session();
    let mut screen = Screen::new();
    let mut text = String::new();
    for line in 0..40 {
        text.push_str(&format!("line {line}\n"));
    }
    feed(
        &mut session,
        &mut screen,
        vec![
            ClientMsg::Resize { cols: 20, rows: 6 },
            typed('i'),
            ClientMsg::Text(text),
            pressed(KeyCode::Esc),
            typed('g'),
            typed('g'),
        ],
    );
    assert_eq!(screen.grid().row_text(0).trim_end(), "line 0");

    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Pointer(PointerMsg::new(
            PointerKind::ScrollDown,
            0,
            0,
        ))],
    );
    assert_eq!(
        screen.grid().row_text(0).trim_end(),
        "line 3",
        "three lines"
    );
    assert_eq!(screen.cursor(), (0, 0), "the cursor did not move");

    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Pointer(PointerMsg::new(
            PointerKind::ScrollUp,
            0,
            0,
        ))],
    );
    assert_eq!(screen.grid().row_text(0).trim_end(), "line 0");
}

/// Copying with nothing selected takes what the register holds.
#[test]
fn copying_without_a_selection_takes_the_register() {
    let mut session = session();
    let mut screen = Screen::new();
    feed(
        &mut session,
        &mut screen,
        vec![
            ClientMsg::Resize { cols: 20, rows: 6 },
            typed('i'),
            ClientMsg::Text("one\ntwo\n".to_string()),
            pressed(KeyCode::Esc),
            typed('g'),
            typed('g'),
        ],
    );
    feed(&mut session, &mut screen, vec![typed('y'), typed('y')]);

    let response = session.handle(ClientMsg::Copy);
    assert_eq!(clipboard(&response), Some("one\n".to_string()));
    assert!(!response.quit);
}

#[test]
fn quit_is_reported_to_the_frontend() {
    let mut session = session();
    session.handle(ClientMsg::Resize { cols: 40, rows: 6 });
    // `:q!` — a clean buffer quits with `:q` too, but be explicit.
    let mut msgs = vec![typed(':'), typed('q'), typed('!')];
    msgs.push(pressed(KeyCode::Enter));
    let mut quit = false;
    for msg in msgs {
        let response = session.handle(msg);
        if response.quit {
            assert!(matches!(response.messages.last(), Some(ServerMsg::Quit)));
            quit = true;
        }
    }
    assert!(quit, "the session never reported that it was over");
}

#[test]
fn shutdown_ends_the_session_without_a_frame() {
    let mut session = session();
    let response = session.handle(ClientMsg::Shutdown);
    assert!(response.quit);
    assert!(response.messages.is_empty());
}

/// The arrow keys, as the frontend sends them: this is the wire contract for
/// every navigation key, in insert mode, where they used to do nothing at all.
#[test]
fn navigation_keys_work_in_insert_mode_over_the_wire() {
    let mut session = session();
    let mut screen = Screen::new();
    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Resize { cols: 20, rows: 6 }],
    );

    let mut keys = vec![typed('i')];
    keys.extend(type_str("ab"));
    keys.push(pressed(KeyCode::Enter));
    keys.extend(type_str("cd"));
    feed(&mut session, &mut screen, keys);
    let grid = screen.grid();
    assert_eq!(grid.row_text(0).trim_end(), "ab");
    assert_eq!(grid.row_text(1).trim_end(), "cd");

    // Left then Up: the cursor walks to the start of the line above.
    feed(
        &mut session,
        &mut screen,
        vec![
            pressed(KeyCode::Left),
            pressed(KeyCode::Up),
            pressed(KeyCode::Home),
        ],
    );
    assert_eq!(
        screen.grid().row_text(0).trim_end(),
        "ab",
        "navigation never edits"
    );

    // `End` lands on `b`, `Left` steps back onto `a`, `Delete` eats it.
    feed(
        &mut session,
        &mut screen,
        vec![
            pressed(KeyCode::End),
            pressed(KeyCode::Left),
            pressed(KeyCode::Delete),
        ],
    );
    assert_eq!(screen.grid().row_text(0).trim_end(), "b");
    assert_eq!(screen.grid().row_text(1).trim_end(), "cd");

    feed(&mut session, &mut screen, vec![pressed(KeyCode::Esc)]);
    assert_eq!(
        screen.cursor(),
        (0, 0),
        "the cursor is on screen: {:?}",
        screen.grid().row_text(0)
    );
}

/// Undo and redo arrive as keys and as named commands, and the screen they
/// leave behind is the restored one — including the line the deletion took with
/// it.
#[test]
fn undo_and_redo_over_the_wire() {
    let mut session = session();
    let mut screen = Screen::new();
    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Resize { cols: 20, rows: 6 }],
    );

    let mut keys = vec![typed('i')];
    keys.extend(type_str("ab"));
    keys.push(pressed(KeyCode::Enter));
    keys.extend(type_str("cd"));
    keys.push(pressed(KeyCode::Esc));
    feed(&mut session, &mut screen, keys);

    // `Gdd`: delete the second line.
    feed(
        &mut session,
        &mut screen,
        vec![typed('G'), typed('d'), typed('d')],
    );
    assert_eq!(screen.grid().row_text(0).trim_end(), "ab");
    assert_eq!(screen.grid().row_text(1).trim_end(), "");

    feed(&mut session, &mut screen, vec![typed('u')]);
    assert_eq!(
        screen.grid().row_text(1).trim_end(),
        "cd",
        "`u` put the line back"
    );
    assert_eq!(screen.grid().row_text(2).trim_end(), "");

    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Input(KeyMsg::ctrl('r'))],
    );
    assert_eq!(
        screen.grid().row_text(1).trim_end(),
        "",
        "`<C-r>` replayed it"
    );

    // The same two by name, which is how a script reaches them.
    let named = |name: &str| ClientMsg::Command {
        name: name.to_string(),
        params: Vec::new(),
    };
    feed(&mut session, &mut screen, vec![named("undo")]);
    assert_eq!(screen.grid().row_text(1).trim_end(), "cd");
    feed(&mut session, &mut screen, vec![named("redo")]);
    assert_eq!(screen.grid().row_text(1).trim_end(), "");
}

#[test]
fn a_resize_after_editing_keeps_the_text() {
    let mut session = session();
    let mut screen = Screen::new();
    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Resize { cols: 20, rows: 4 }],
    );
    let mut keys = vec![typed('i')];
    keys.extend(type_str("abcdefghijklmnop"));
    keys.push(pressed(KeyCode::Esc));
    feed(&mut session, &mut screen, keys);

    // A resize makes the old screen the wrong shape to patch, so this is a
    // whole frame again.
    let messages = frames(&mut session, ClientMsg::Resize { cols: 60, rows: 8 });
    assert!(
        matches!(messages[..], [ServerMsg::Redraw { .. }]),
        "a resize needs a whole screen: {messages:?}"
    );
    fold(
        &Response {
            messages,
            quit: false,
        },
        &mut screen,
    );

    let grid = screen.grid();
    assert_eq!(grid.width, 60);
    assert_eq!(grid.height, 8);
    assert_eq!(grid.row_text(0).trim_end(), "abcdefghijklmnop");
}

/// The language-server path through the real binary: an init file that turns the
/// setting on, a file that arrives over the wire *after* the daemon started, and
/// a question that has to come back with the fixture's answer on the message row.
///
/// This is the path a frontend takes, and the one nothing else covers: the session
/// is built before the file is opened, so a language server decided only at
/// startup would never see a document.
#[test]
fn end_to_end_with_a_language_server() {
    let fixture = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
        .and_then(|dir| dir.parent().map(|dir| dir.to_path_buf()))
        .map(|dir| dir.join(format!("fake-lsp-server{}", std::env::consts::EXE_SUFFIX)));
    let Some(fixture) = fixture.filter(|path| path.is_file()) else {
        return;
    };

    let dir = temp_dir("lsp-e2e");
    let init = dir.join("lsp-init.lua");
    std::fs::write(&init, "ed.set(\"lsp\", true)\n").unwrap();
    let path = dir.join("asked.rs");
    std::fs::write(&path, "fn target_name() {}\n").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_inkstone-server"))
        .arg("--init")
        .arg(&init)
        .env("INKSTONE_LSP", &fixture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn inkstone-server");

    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel::<ServerMsg>();
    let reader = thread::spawn(move || {
        let mut input = BufReader::new(stdout);
        while let Ok(Some(msg)) = read_server_msg(&mut input) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });

    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut send = |msg: &ClientMsg| {
        write_client_msg(&mut stdin, msg).expect("failed to write a frame");
        stdin.flush().expect("failed to flush");
    };
    let mut screen = Screen::new();
    // Drain whatever the daemon has sent, folding each frame into the screen the
    // way a frontend does.
    let drain = |rx: &mpsc::Receiver<ServerMsg>, screen: &mut Screen| {
        while let Ok(msg) = rx.recv_timeout(Duration::from_millis(300)) {
            fold_frame(&msg, screen);
        }
    };

    drain(&rx, &mut screen);
    send(&ClientMsg::Resize { cols: 60, rows: 8 });
    drain(&rx, &mut screen);
    // What the session said about the server it was asked to start: a failure to
    // start one is a message, and the file-open message would cover it later.
    let startup = {
        let grid = screen.grid();
        grid.row_text(grid.height - 1).trim().to_string()
    };
    assert!(
        !startup.starts_with("lsp:"),
        "the server did not start: {startup:?}"
    );

    // The file arrives the way a frontend opens it.
    send(&ClientMsg::Command {
        name: "edit_file".to_string(),
        params: vec![Param::Str(path.to_string_lossy().to_string())],
    });
    drain(&rx, &mut screen);

    // `K` asks the server what the word under the cursor is.
    send(&typed('K'));
    let deadline = std::time::Instant::now() + TIMEOUT;
    let mut said = String::new();
    while std::time::Instant::now() < deadline {
        drain(&rx, &mut screen);
        let grid = screen.grid();
        said = grid.row_text(grid.height - 1).trim().to_string();
        if said.contains("fixture") {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        said.contains("fixture"),
        "the server's answer never reached the message row: {said:?}"
    );

    // The same question for the completion list: typing a word and asking for
    // completions gets the server's items, each one resolved for its details.
    for key in type_str("ofix") {
        send(&key);
    }
    drain(&rx, &mut screen);
    send(&ClientMsg::Input(KeyMsg::ctrl('n')));
    let deadline = std::time::Instant::now() + TIMEOUT;
    let mut popup = String::new();
    while std::time::Instant::now() < deadline {
        drain(&rx, &mut screen);
        let grid = screen.grid();
        popup = (0..grid.height)
            .map(|row| grid.row_text(row))
            .collect::<Vec<String>>()
            .join("\n");
        if popup.contains("fixture_label") && popup.contains("the fixture's word for") {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        popup.contains("fixture_label"),
        "the server's items never reached the popup:\n{popup}"
    );
    assert!(
        popup.contains("the fixture's word for fixture_"),
        "the resolved detail never reached the popup:\n{popup}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
}

/// The rows of the screen, top to bottom.
fn rows(screen: &Screen) -> Vec<String> {
    let grid = screen.grid();
    (0..grid.height).map(|row| grid.row_text(row)).collect()
}

/// The daemon's own turns, until `done` says so or the timeout runs out.
///
/// A command runs on threads of its own and its answer arrives at a tick, so a
/// test that wants the answer has to tick the way the daemon's loop does.
fn pump(session: &mut Session, screen: &mut Screen, done: impl Fn(&Screen) -> bool) -> bool {
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        if done(screen) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        session.tick();
        if let Some(frame) = session.frame_if_dirty() {
            fold_frame(&frame, screen);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_command_runs_and_what_it_says_becomes_a_document() {
    let mut session = session();
    let mut screen = Screen::new();
    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Resize { cols: 60, rows: 12 }],
    );

    let mut keys = type_str(":!echo hello");
    keys.push(pressed(KeyCode::Enter));
    feed(&mut session, &mut screen, keys);

    assert!(
        pump(&mut session, &mut screen, |screen| {
            rows(screen).iter().any(|row| row.starts_with("hello"))
        }),
        "the output is on screen: {:?}",
        rows(&screen)
    );
    // In a window of its own, named after the command that produced it.
    assert!(
        rows(&screen).iter().any(|row| row.contains("!echo hello")),
        "and the window says where it came from: {:?}",
        rows(&screen)
    );
}

#[test]
fn a_filter_runs_the_text_through_a_command() {
    let dir = temp_dir("filter");
    let path = dir.join("filtered.txt");
    std::fs::write(&path, "pear\napple\n").unwrap();

    let mut session = session();
    let mut screen = Screen::new();
    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Resize { cols: 60, rows: 8 }],
    );
    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Command {
            name: "edit_file".to_string(),
            params: vec![Param::Str(path.to_string_lossy().to_string())],
        }],
    );

    // `:%!sort` — `sort` is a command both platforms' shells have, and sorting
    // two lines is the same answer whichever one runs it.
    let mut keys = type_str(":%!sort");
    keys.push(pressed(KeyCode::Enter));
    feed(&mut session, &mut screen, keys);

    assert!(
        pump(&mut session, &mut screen, |screen| {
            rows(screen)
                .last()
                .is_some_and(|row| row.trim_end().ends_with("lines filtered"))
        }),
        "the filter was applied: {:?}",
        rows(&screen)
    );
    assert!(
        rows(&screen)[0].trim_end() == "apple",
        "the text is the sorted one: {:?}",
        rows(&screen)
    );
    // What the buffer holds is what a write puts on disk, which is the whole
    // point of a filter.
    let mut keys = type_str(":wq");
    keys.push(pressed(KeyCode::Enter));
    feed(&mut session, &mut screen, keys);

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "apple\npear\n",
        "the file on disk is the filtered text"
    );
}

#[test]
fn a_command_that_will_not_finish_can_be_killed() {
    // A command that takes longer than the test: `ping` on Windows, `sleep`
    // everywhere else, both of which do nothing but take time.
    #[cfg(windows)]
    let slow = "ping -n 30 127.0.0.1";
    #[cfg(not(windows))]
    let slow = "sleep 30";

    let mut session = session();
    let mut screen = Screen::new();
    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Resize { cols: 60, rows: 10 }],
    );
    let mut keys = type_str(&format!(":!{slow}"));
    keys.push(pressed(KeyCode::Enter));
    feed(&mut session, &mut screen, keys);
    assert!(
        pump(&mut session, &mut screen, |screen| {
            rows(screen).last().is_some_and(|row| row.starts_with('!'))
        }),
        "the command was taken: {:?}",
        rows(&screen)
    );

    let mut keys = type_str(":kill");
    keys.push(pressed(KeyCode::Enter));
    feed(&mut session, &mut screen, keys);

    assert!(
        pump(&mut session, &mut screen, |screen| {
            rows(screen)
                .last()
                .is_some_and(|row| row.starts_with("shell "))
        }),
        "the answer is that it was stopped: {:?}",
        rows(&screen)
    );
    // And the editor is free: the command after it is not refused.
    let mut keys = type_str(":!echo after");
    keys.push(pressed(KeyCode::Enter));
    feed(&mut session, &mut screen, keys);
    assert!(
        pump(&mut session, &mut screen, |screen| {
            rows(screen).iter().any(|row| row.starts_with("after"))
        }),
        "another command runs: {:?}",
        rows(&screen)
    );
}

#[test]
fn a_script_runs_a_command_and_acts_on_what_it_said() {
    let dir = temp_dir("lua-job");
    let init = dir.join("job-init.lua");
    std::fs::write(
        &init,
        r#"
ed.shell("echo from lua", function(params)
    ed.run("normal", { "i" .. params[1] .. "<Esc>" })
end)
"#,
    )
    .unwrap();

    let mut session = Session::new(Editor::new(Some(&init)).expect("editor"));
    let mut screen = Screen::new();
    feed(
        &mut session,
        &mut screen,
        vec![ClientMsg::Resize { cols: 40, rows: 6 }],
    );

    // The command runs, and the callback types what it said into the buffer —
    // which is the whole story of a script with hands.
    assert!(
        pump(&mut session, &mut screen, |screen| {
            rows(screen).iter().any(|row| row.trim_end() == "from lua")
        }),
        "the output was typed into the buffer: {:?}",
        rows(&screen)
    );
}

/// The real thing: spawn `inkstone-server`, talk to it over its pipes, then check
/// that the file it wrote is on disk.
#[test]
fn end_to_end_over_stdio() {
    let dir = temp_dir("e2e");
    let path = dir.join("e2e.txt");
    let _ = std::fs::remove_file(&path);

    let mut child = Command::new(env!("CARGO_BIN_EXE_inkstone-server"))
        .arg("--no-init")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn inkstone-server");

    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel::<ServerMsg>();
    let reader = thread::spawn(move || {
        let mut input = BufReader::new(stdout);
        while let Ok(Some(msg)) = read_server_msg(&mut input) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });

    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut send = |msg: &ClientMsg| {
        write_client_msg(&mut stdin, msg).expect("failed to write a frame");
        stdin.flush().expect("failed to flush");
    };

    let mut screen = Screen::new();

    // The daemon sends a whole screen as soon as it starts.
    let first = rx.recv_timeout(TIMEOUT).expect("no initial frame");
    match &first {
        ServerMsg::Redraw {
            grid,
            cursor_visible,
            ..
        } => {
            assert_eq!(grid.width, 80, "default viewport before any resize");
            assert!(grid.row_text(grid.height - 2).starts_with("-- NORMAL --"));
            assert!(cursor_visible);
        }
        other => panic!("expected a redraw, got {other:?}"),
    }
    fold_frame(&first, &mut screen);

    send(&ClientMsg::Resize { cols: 40, rows: 6 });
    let frame = rx.recv_timeout(TIMEOUT).expect("no frame after resize");
    match &frame {
        ServerMsg::Redraw { grid, .. } => assert_eq!((grid.width, grid.height), (40, 6)),
        other => panic!("expected a redraw, got {other:?}"),
    }
    fold_frame(&frame, &mut screen);

    // Type "hello" in insert mode, then leave normal mode.
    let mut keys = vec![ClientMsg::Input(KeyMsg::new(KeyCode::Char('i')))];
    for c in "hello".chars() {
        keys.push(ClientMsg::Input(KeyMsg::new(KeyCode::Char(c))));
    }
    keys.push(ClientMsg::Input(KeyMsg::new(KeyCode::Esc)));

    let mut seen_hello = false;
    for msg in keys {
        send(&msg);
        let frame = rx.recv_timeout(TIMEOUT).expect("no frame for a key");
        fold_frame(&frame, &mut screen);
        seen_hello |= screen.grid().row_text(0).trim_end() == "hello";
    }
    assert!(seen_hello, "typing never showed up on screen");
    assert!(
        screen.grid().row_text(4).contains("NORMAL"),
        "escape returned to normal mode: {:?}",
        screen.grid().row_text(4)
    );

    // `dd` takes the line out and `u` puts it back, in the real process: if
    // the history were wrong, the file would be written out empty.
    for c in "ddu".chars() {
        send(&ClientMsg::Input(KeyMsg::new(KeyCode::Char(c))));
        let frame = rx.recv_timeout(TIMEOUT).expect("no frame for a key");
        fold_frame(&frame, &mut screen);
    }
    assert_eq!(
        screen.grid().row_text(0).trim_end(),
        "hello",
        "the line came back"
    );

    // Write it out, then close stdin: the daemon should notice EOF and exit.
    send(&ClientMsg::Command {
        name: "save".into(),
        params: vec![Param::Str(path.to_string_lossy().into_owned())],
    });
    rx.recv_timeout(TIMEOUT).expect("no frame after save");

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");

    drop(stdin);
    let status = wait_with_timeout(&mut child, TIMEOUT);
    assert!(status.success(), "inkstone-server exited with {status:?}");
    reader.join().expect("the reader thread panicked");
}

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("inkstone-server did not exit within {timeout:?}");
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("failed to poll inkstone-server: {e}"),
        }
    }
}
