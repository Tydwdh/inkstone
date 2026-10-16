//! Where a path the user names leads: the session's directory.
//!
//! The process's directory never moves — the daemon was started somewhere and
//! that is that — so a relative path has to be relative to something the editor
//! owns. That is this: `:cd`, and the directory a listing is showing, because the
//! directory a person is looking at is the one they mean.

use super::*;

fn a_tree(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src").join("main.rs"), "the real main\n").unwrap();
    std::fs::write(dir.join("t.txt"), "root file\n").unwrap();
    dir
}

#[test]
fn the_session_starts_where_the_process_does() {
    let mut ed = Editor::for_tests();
    assert_eq!(
        ed.cwd(),
        std::env::current_dir().unwrap(),
        "a relative path means what it means to the process until something moves it"
    );
    ed.execute_cmdline("cd");
    assert!(
        !ed.cwd().as_os_str().is_empty(),
        "and `:cd` with no argument is home"
    );
}

#[test]
fn a_listing_moves_the_session_into_the_directory_it_shows() {
    let dir = a_tree("inkstone-paths-listing");

    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("e {}", dir.join("src").display()));

    assert!(ed.buffer.is_listing());
    assert_eq!(ed.cwd(), dir.join("src"), "looking at it is being in it");
    // And walking out of it moves again: `-` is a change of place too.
    feed(&mut ed, "-");
    assert_eq!(ed.cwd(), dir);
}

#[test]
fn a_name_typed_in_a_listing_is_beside_it() {
    // The report this was built from: inside a listing of `src/`, `:e main.rs`
    // opened an empty new buffer in the daemon's own directory.
    let dir = a_tree("inkstone-paths-relative");

    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("e {}", dir.join("src").display()));
    ed.execute_cmdline("e main.rs");

    assert_eq!(ed.buffer.display_name(), "main.rs");
    assert_eq!(
        ed.buffer.text(),
        "the real main\n",
        "the file beside the listing, not a new one where the daemon was started"
    );
    assert_eq!(
        ed.buffer.path(),
        Some(dir.join("src").join("main.rs").as_path())
    );
}

#[test]
fn a_write_lands_where_the_name_was_read_from() {
    let dir = a_tree("inkstone-paths-write");

    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("e {}", dir.join("src").display()));
    ed.execute_cmdline("e made-up.txt");
    // Typed into the new buffer, so that the document keeps the path `:e` gave
    // it: replacing the buffer would replace where it is, too.
    feed(&mut ed, "iwritten<Esc>");
    ed.execute_cmdline("w");

    assert_eq!(
        std::fs::read_to_string(dir.join("src").join("made-up.txt")).unwrap(),
        "written\n",
        "beside the listing"
    );
    assert!(
        !dir.join("made-up.txt").exists(),
        "and not in the directory the daemon was started in"
    );
}

#[test]
fn read_a_file_by_a_name_relative_to_the_session() {
    let dir = a_tree("inkstone-paths-read");

    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("e {}", dir.join("src").display()));
    // One directory up and in, by name: the listing's own line is still there,
    // and the file it names arrives below it.
    ed.execute_cmdline("r ../t.txt");
    assert_eq!(ed.buffer.text(), "main.rs\nroot file\n");
}

#[test]
fn gf_looks_for_a_name_where_the_session_is() {
    let dir = a_tree("inkstone-paths-gf");

    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("cd {}", dir.display()));
    // A buffer with no file around it: the name stands on its own, and standing
    // on its own means standing where the session is.
    ed.buffer = crate::buffer::Buffer::from_text("see t.txt\n");
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "gf");

    assert_eq!(ed.buffer.text(), "root file\n");
    assert_eq!(ed.buffer.path(), Some(dir.join("t.txt").as_path()));
}

#[test]
fn cd_moves_it_and_pwd_says_where_it_is() {
    let dir = a_tree("inkstone-paths-cd");

    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("cd {}", dir.display()));
    assert_eq!(ed.cwd(), dir);

    ed.execute_cmdline("pwd");
    assert_eq!(ed.message, dir.display().to_string());

    // A directory named relative to where the session already is.
    ed.execute_cmdline("cd src");
    assert_eq!(ed.cwd(), dir.join("src"));

    // And one that is not there is refused rather than half-taken.
    ed.execute_cmdline("cd nowhere");
    assert!(
        ed.message.starts_with("E344"),
        "a directory that is not there: {}",
        ed.message
    );
    assert_eq!(ed.cwd(), dir.join("src"), "and the session did not move");
}

#[test]
fn an_absolute_path_is_left_alone() {
    let dir = a_tree("inkstone-paths-absolute");

    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("cd {}", dir.display()));
    ed.execute_cmdline(&format!("e {}", dir.join("t.txt").display()));
    assert_eq!(ed.buffer.path(), Some(dir.join("t.txt").as_path()));
    assert_eq!(ed.buffer.text(), "root file\n");
}
