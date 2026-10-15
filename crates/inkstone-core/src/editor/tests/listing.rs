//! A directory, opened as the list of what is in it.
//!
//! The tests here are about the *reading* half: what a listing shows, and where
//! the two keys that navigate one go. What writing one does lives in
//! `buffer.rs`, beside the code that decides it.

use super::*;
use crate::buffer::Buffer;

/// What the status line shows for the listing of `dir`: the path as it was
/// given, with the platform's separator, which is what says it is a directory.
fn shown(dir: &std::path::Path) -> String {
    format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR)
}

/// A directory of the shape these tests want: two directories (so that "in
/// order" is visible) and one file, in its own path so that the other test
/// files cannot write into it half way through.
fn a_tree(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("seed.txt"), "the seed\n").unwrap();
    dir
}

#[test]
fn a_directory_opens_as_a_listing() {
    let dir = a_tree("inkstone-listing-opens");

    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("e {}", dir.display()));

    assert_eq!(
        ed.buffer.text(),
        "docs/\nsub/\nseed.txt\n",
        "directories first"
    );
    assert_eq!(ed.buffer.display_name(), shown(&dir));
    // Opening a directory is opening a document, like any other file: the
    // status line says where a person is and the message says what arrived.
    assert_eq!(ed.message, format!("\"{}\"", shown(&dir)));
    assert!(!ed.is_dirty(), "a listing arrives as it is on disk");
}

#[test]
fn an_empty_directory_lists_nothing_and_says_so() {
    let dir = a_tree("inkstone-listing-empty");
    std::fs::remove_file(dir.join("seed.txt")).unwrap();
    std::fs::remove_dir_all(dir.join("docs")).unwrap();
    std::fs::remove_dir_all(dir.join("sub")).unwrap();

    let mut ed = Editor::for_tests();
    ed.open_file(&dir).unwrap();
    assert_eq!(ed.buffer.text(), "\n");
    assert_eq!(ed.buffer.line_count(), 1);
}

#[test]
fn the_line_under_the_cursor_is_the_entry_it_opens() {
    let dir = a_tree("inkstone-listing-enter");
    std::fs::write(dir.join("sub").join("inside.txt"), "inside\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.open_file(&dir).unwrap();
    assert_eq!(ed.buffer.text(), "docs/\nsub/\nseed.txt\n");

    // The cursor starts on `docs/`, which has nothing in it.
    feed(&mut ed, "<CR>");
    assert_eq!(ed.buffer.display_name(), shown(&dir.join("docs")));
    assert_eq!(ed.buffer.text(), "\n");

    // Up, and into `sub/`, which holds a file.
    feed(&mut ed, "-");
    assert_eq!(ed.buffer.display_name(), shown(&dir));
    feed(&mut ed, "j<CR>");
    assert_eq!(ed.buffer.display_name(), shown(&dir.join("sub")));
    assert_eq!(ed.buffer.text(), "inside.txt\n");

    // A file from a listing is a document of its own: the listing stays behind,
    // where `:b` finds it again.
    let opened = ed.documents.len();
    feed(&mut ed, "<CR>");
    assert_eq!(ed.buffer.display_name(), "inside.txt");
    assert_eq!(ed.buffer.text(), "inside\n");
    assert_eq!(ed.documents.len(), opened + 1, "the listing is still open");
}

#[test]
fn walking_down_and_back_up_is_one_document() {
    let dir = a_tree("inkstone-listing-walk");
    let mut ed = Editor::for_tests();
    ed.open_file(&dir).unwrap();

    let before = ed.documents.len();
    feed(&mut ed, "<CR>");
    assert_eq!(
        ed.documents.len(),
        before,
        "a directory takes the listing's place"
    );
    feed(&mut ed, "-");
    assert_eq!(ed.buffer.display_name(), shown(&dir));
    assert_eq!(
        ed.documents.len(),
        before,
        "and coming back up is the same one"
    );
}

#[test]
fn the_cursor_of_a_listing_start_at_the_top_with_nothing_held() {
    // `<CR>` on the line an empty listing holds does nothing rather than
    // naming a file called "".
    let dir = a_tree("inkstone-listing-blank");
    std::fs::remove_file(dir.join("seed.txt")).unwrap();
    std::fs::remove_dir_all(dir.join("docs")).unwrap();
    std::fs::remove_dir_all(dir.join("sub")).unwrap();

    let mut ed = Editor::for_tests();
    ed.open_file(&dir).unwrap();
    feed(&mut ed, "<CR>");
    assert_eq!(ed.buffer.display_name(), shown(&dir));
    assert!(ed.message.is_empty(), "and says nothing: {}", ed.message);
}

#[test]
fn minus_is_a_line_up_anywhere_but_a_listing() {
    let dir = a_tree("inkstone-listing-minus");
    let mut ed = Editor::for_tests();
    ed.open_file(&dir.join("seed.txt")).unwrap();
    assert_eq!(ed.cursor.row, 0);

    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.cursor.row = 1;
    feed(&mut ed, "-");
    assert_eq!(ed.cursor.row, 0, "vim's motion, on an ordinary buffer");
}

#[test]
fn gf_opens_a_directory_as_a_listing_too() {
    // The name under the cursor is resolved by the same code that opens a file,
    // so a directory it resolves to arrives as a listing without `gf` knowing
    // anything about listings.
    let dir = a_tree("inkstone-listing-gf");
    let readme = dir.join("readme.txt");
    std::fs::write(&readme, "see sub\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.open_file(&readme).unwrap();
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "gf");

    assert_eq!(ed.buffer.display_name(), shown(&dir.join("sub")));
    // `gf` names what it opened, the way it does for a file.
    assert_eq!(
        ed.message,
        format!("\"{}\"", dir.join("sub").display()),
        "no refusal"
    );
}

#[test]
fn a_listing_edited_like_text_makes_files_like_text() {
    let dir = a_tree("inkstone-listing-write");
    let mut ed = Editor::for_tests();
    ed.open_file(&dir).unwrap();
    assert_eq!(ed.buffer.text(), "docs/\nsub/\nseed.txt\n");

    // Two names typed in at the end, the way a person adds entries to a
    // listing: a file, and a directory because of the `/`.
    feed(&mut ed, "Gonew.txt<Esc>otools/<Esc>");
    assert!(
        ed.is_dirty(),
        "a listing that was edited is a document that was"
    );
    feed(&mut ed, ":w<CR>");
    assert!(!ed.is_dirty(), "and writing it is what makes it clean");

    assert!(dir.join("new.txt").is_file());
    assert!(dir.join("tools").is_dir());
    assert_eq!(
        std::fs::metadata(dir.join("new.txt")).unwrap().len(),
        0,
        "an entry with no content is an empty file, not a missing one"
    );
    // The message is the path, the way it is for a file: the status line is
    // where the display name goes.
    assert_eq!(ed.message, format!("\"{}\" written", dir.display()));

    // And taking a name out of the listing takes the entry out of the
    // directory.
    feed(&mut ed, "ggdd:w<CR>");
    assert!(!dir.join("docs").exists());
    assert!(dir.join("sub").is_dir(), "the rest is where it was");
}

#[test]
fn walking_away_from_an_edited_listing_is_refused() {
    let dir = a_tree("inkstone-listing-dirty");
    let mut ed = Editor::for_tests();
    ed.open_file(&dir).unwrap();

    // Take an entry out of the listing, and try to walk into another
    // directory with the change unwritten.
    feed(&mut ed, "Gdd");
    assert!(ed.is_dirty());
    assert_eq!(ed.buffer.text(), "docs/\nsub/\n");
    feed(&mut ed, "<CR>");
    assert_eq!(
        ed.message, "E37: No write since last change (add ! to override)",
        "the listing did not go anywhere"
    );
    assert_eq!(
        ed.buffer.text(),
        "docs/\nsub/\n",
        "and neither did the edit"
    );
    feed(&mut ed, "-");
    assert!(ed.message.starts_with("E37:"), "{}", ed.message);
    assert_eq!(ed.buffer.display_name(), shown(&dir));

    // `:w` is what makes it stop refusing, and it does what the lines said.
    feed(&mut ed, ":w<CR>");
    assert!(!dir.join("seed.txt").exists());
    feed(&mut ed, "gg<CR>");
    assert_eq!(ed.buffer.display_name(), shown(&dir.join("docs")));
}

#[test]
fn an_unreadable_directory_is_an_error_and_not_a_listing() {
    // A path that is not there is still a new, empty file — the branch that
    // reads a directory must not swallow it.
    let dir = a_tree("inkstone-listing-missing");
    let mut ed = Editor::for_tests();
    ed.open_file(&dir.join("nothing.txt")).unwrap();
    assert_eq!(ed.buffer.text(), "\n");
    assert_eq!(ed.buffer.display_name(), "nothing.txt");
    assert!(!ed.buffer.is_listing());
}
