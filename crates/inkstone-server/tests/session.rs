//! Integration tests for the daemon: the in-process session seam, plus one
//! end-to-end run against the real `inkstone-server` binary over its stdio pipes.

use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use inkstone_core::Editor;
use inkstone_rpc::{
    read_server_msg, write_client_msg, ClientMsg, Grid, KeyCode, KeyMsg, Param, ServerMsg,
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

fn redraw(response: &Response) -> Option<&Grid> {
    response.messages.iter().find_map(|msg| match msg {
        ServerMsg::Redraw { grid, .. } => Some(grid),
        _ => None,
    })
}

fn last_redraw(session: &mut Session, msgs: Vec<ClientMsg>) -> Grid {
    let mut latest = None;
    for msg in msgs {
        let response = session.handle(msg);
        if let Some(grid) = redraw(&response) {
            latest = Some(grid.clone());
        }
    }
    match latest {
        Some(grid) => grid,
        None => panic!("no redraw was produced"),
    }
}

fn session() -> Session {
    Session::new(Editor::new(None).expect("editor"))
}

#[test]
fn resize_produces_a_frame_of_the_requested_size() {
    let mut session = session();
    let response = session.handle(ClientMsg::Resize { cols: 40, rows: 6 });
    let grid = redraw(&response).expect("a redraw");
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
    // Wide enough that the `written` message is not truncated.
    session.handle(ClientMsg::Resize { cols: 120, rows: 6 });

    let mut keys = vec![typed('i')];
    keys.extend(type_str("hello"));
    keys.push(pressed(KeyCode::Esc));
    let grid = last_redraw(&mut session, keys);
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
    let grid = redraw(&response).expect("a redraw");
    assert!(
        grid.row_text(5).contains("written"),
        "message row: {:?}",
        grid.row_text(5)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
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
    session.handle(ClientMsg::Resize { cols: 20, rows: 6 });

    let mut keys = vec![typed('i')];
    keys.extend(type_str("ab"));
    keys.push(pressed(KeyCode::Enter));
    keys.extend(type_str("cd"));
    let grid = last_redraw(&mut session, keys);
    assert_eq!(grid.row_text(0).trim_end(), "ab");
    assert_eq!(grid.row_text(1).trim_end(), "cd");

    // Left then Up: the cursor walks to the start of the line above.
    let mut keys = vec![pressed(KeyCode::Left), pressed(KeyCode::Up)];
    keys.push(pressed(KeyCode::Home));
    let grid = last_redraw(&mut session, keys);
    assert_eq!(grid.row_text(0).trim_end(), "ab", "navigation never edits");

    // `End` lands on `b`, `Left` steps back onto `a`, `Delete` eats it.
    let grid = last_redraw(
        &mut session,
        vec![
            pressed(KeyCode::End),
            pressed(KeyCode::Left),
            pressed(KeyCode::Delete),
        ],
    );
    assert_eq!(grid.row_text(0).trim_end(), "b");
    assert_eq!(grid.row_text(1).trim_end(), "cd");
    match last_redraw_reply(&mut session, vec![pressed(KeyCode::Esc)]) {
        ServerMsg::Redraw { cursor, grid, .. } => {
            assert_eq!(
                cursor,
                (0, 0),
                "the cursor is on screen: {:?}",
                grid.row_text(0)
            );
        }
        other => panic!("expected a redraw, got {other:?}"),
    }
}

fn last_redraw_reply(session: &mut Session, msgs: Vec<ClientMsg>) -> ServerMsg {
    let mut latest = None;
    for msg in msgs {
        let response = session.handle(msg);
        if let Some(msg) = response
            .messages
            .into_iter()
            .rev()
            .find(|m| matches!(m, ServerMsg::Redraw { .. }))
        {
            latest = Some(msg);
        }
    }
    latest.expect("no redraw was produced")
}

#[test]
fn a_resize_after_editing_keeps_the_text() {
    let mut session = session();
    session.handle(ClientMsg::Resize { cols: 20, rows: 4 });
    let mut keys = vec![typed('i')];
    keys.extend(type_str("abcdefghijklmnop"));
    keys.push(pressed(KeyCode::Esc));
    last_redraw(&mut session, keys);

    let response = session.handle(ClientMsg::Resize { cols: 60, rows: 8 });
    let grid = redraw(&response).expect("a redraw");
    assert_eq!(grid.width, 60);
    assert_eq!(grid.height, 8);
    assert_eq!(grid.row_text(0).trim_end(), "abcdefghijklmnop");
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

    // The daemon sends a frame as soon as it starts.
    let first = rx.recv_timeout(TIMEOUT).expect("no initial frame");
    match first {
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

    send(&ClientMsg::Resize { cols: 40, rows: 6 });
    let frame = rx.recv_timeout(TIMEOUT).expect("no frame after resize");
    match frame {
        ServerMsg::Redraw { grid, .. } => assert_eq!((grid.width, grid.height), (40, 6)),
        other => panic!("expected a redraw, got {other:?}"),
    }

    // Type "hello" in insert mode, then leave normal mode.
    let mut keys = vec![ClientMsg::Input(KeyMsg::new(KeyCode::Char('i')))];
    for c in "hello".chars() {
        keys.push(ClientMsg::Input(KeyMsg::new(KeyCode::Char(c))));
    }
    keys.push(ClientMsg::Input(KeyMsg::new(KeyCode::Esc)));

    let mut seen_hello = false;
    let mut last_grid = None;
    for msg in keys {
        send(&msg);
        let frame = rx.recv_timeout(TIMEOUT).expect("no frame for a key");
        if let ServerMsg::Redraw { grid, .. } = frame {
            seen_hello |= grid.row_text(0).trim_end() == "hello";
            last_grid = Some(grid);
        }
    }
    assert!(seen_hello, "typing never showed up on screen");
    let last_grid = last_grid.expect("a redraw");
    assert!(
        last_grid.row_text(4).contains("NORMAL"),
        "escape returned to normal mode: {:?}",
        last_grid.row_text(4)
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
