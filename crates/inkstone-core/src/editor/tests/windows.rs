use super::*;

#[test]
fn the_item_the_list_moves_to_is_the_one_asked_about() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "i");
    ed.type_text("o");
    ed.set_completions(vec![
        CompletionItem {
            label: "one".to_string(),
            insert: "one".to_string(),
            detail: String::new(),
            snippet: false,
        },
        CompletionItem {
            label: "other".to_string(),
            insert: "other".to_string(),
            detail: String::new(),
            snippet: false,
        },
    ]);
    // Opening the list selects one, and that one is worth asking about.
    let asked = ed.take_completion_resolve();
    assert!(asked.is_some(), "an item was selected");
    assert!(ed.take_completion_resolve().is_none(), "and taken once");
    // Moving to the next asks about that one instead.
    feed(&mut ed, "<C-n>");
    let next = ed.take_completion_resolve().expect("the next item");
    assert_ne!(Some(next.clone()), asked, "a different item");
    // What comes back lands on both copies of the item and asks for a frame.
    ed.redraw = false;
    ed.set_completion_detail(&next, "a word the server knows");
    assert!(ed.redraw, "the popup changed, so a frame is due");
    let state = ed.completion.as_ref().expect("a list is up");
    let shown = state
        .shown
        .iter()
        .find(|item| item.label == next)
        .expect("the shown copy");
    assert_eq!(shown.detail, "a word the server knows");
    let all = state
        .all
        .iter()
        .find(|item| item.label == next)
        .expect("the copy of everything");
    assert_eq!(all.detail, "a word the server knows");
    // Something nobody sent is not invented.
    ed.redraw = false;
    ed.set_completion_detail("nothing", "made up");
    assert!(!ed.redraw, "an item that is not there is not made up");
}

#[test]
fn the_list_of_places_can_be_shown_in_a_window() {
    let dir = std::env::temp_dir().join("inkstone-copen");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("one.txt");
    let two = dir.join("two.txt");
    std::fs::write(&one, "first\nsecond\n").unwrap();
    std::fs::write(&two, "third\nfourth\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("mine\n");
    // With no list there is nothing to open, and no window appears.
    ed.execute_cmdline("copen");
    assert!(ed.message.contains("E776"), "{}", ed.message);
    assert_eq!(ed.windows.len(), 1);

    ed.set_places(vec![(one.clone(), 0, 0), (two.clone(), 1, 1)]);
    ed.execute_cmdline("copen");
    assert_eq!(ed.windows.len(), 2, "a window of its own");
    assert_eq!(ed.list, Some(List::Places), "and it is a list, not a file");
    let text = ed.buffer.text();
    assert!(text.starts_with("1 "), "{text:?}");
    assert!(text.contains("one.txt"), "{text:?}");
    assert_eq!(ed.message, "2 places");
    // The lines are the places, in order: two places, two lines.
    assert_eq!(ed.buffer.line_count(), 2);

    // A list is not edited.
    let before = ed.buffer.text();
    feed(&mut ed, "x");
    assert_eq!(ed.buffer.text(), before);
    assert!(ed.message.starts_with("E21"), "{}", ed.message);

    // `<CR>` on a line goes to the place that line names, and closes the
    // window it was reading.
    feed(&mut ed, "j");
    feed(&mut ed, "<CR>");
    assert_eq!(ed.windows.len(), 1, "the list window went away");
    assert_eq!(ed.list, None);
    assert!(
        ed.buffer.path().is_some_and(|path| path == two),
        "and we are at the second place: {:?}",
        ed.buffer.path()
    );
    assert_eq!(ed.cursor.row, 1);
    assert_eq!(ed.cursor.col, 1);

    // `:copen` again, and `:cclose` closes it without going anywhere.
    ed.execute_cmdline("copen");
    assert_eq!(ed.windows.len(), 2);
    ed.execute_cmdline("cclose");
    assert_eq!(ed.windows.len(), 1);
    // Anywhere but a list, `<CR>` is the motion it always was.
    assert_eq!(ed.list, None);
    ed.execute_cmdline("cclose");
    assert!(ed.message.contains("E776"), "{}", ed.message);
}

#[test]
fn a_file_that_changed_underneath_is_not_overwritten_by_accident() {
    let dir = std::env::temp_dir().join("inkstone-stamp");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("shared.txt");
    std::fs::write(&path, "mine\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.open_file(&path).unwrap();
    ed.type_text("more\n"); // at the cursor, which is the top of the file
                            // Something else writes the file: the next `:w` refuses.
    std::fs::write(&path, "theirs\n").unwrap();
    ed.execute_cmdline("w");
    assert!(
        ed.message.starts_with("E13"),
        "the write is refused: {}",
        ed.message
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "theirs\n",
        "and their text is still there"
    );
    // `:w!` says a person knows, and writes anyway.
    ed.execute_cmdline("w!");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "more\nmine\n");
    // And after writing, the stamp is the new one: another `:w` is fine.
    ed.execute_cmdline("w");
    assert!(
        ed.message.contains("written"),
        "nothing changed since: {}",
        ed.message
    );
    // A file that did not exist has nothing to compare with, so it saves.
    let fresh = dir.join("fresh.txt");
    let _ = std::fs::remove_file(&fresh);
    let mut ed = Editor::for_tests();
    ed.open_file(&fresh).unwrap();
    ed.type_text("hello"); // a new file is one empty line, so no break of its own
    ed.execute_cmdline("w");
    assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "hello\n");
}

#[test]
fn the_lists_a_session_showed_can_be_walked_back_through() {
    let dir = std::env::temp_dir().join("inkstone-colder");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("one.txt");
    let two = dir.join("two.txt");
    std::fs::write(&one, "a\n").unwrap();
    std::fs::write(&two, "b\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("mine\n");
    // Nothing shown yet.
    ed.execute_cmdline("colder");
    assert!(ed.message.contains("E776"), "{}", ed.message);
    // Two questions, so two lists.
    ed.set_places(vec![(one.clone(), 0, 0)]);
    ed.set_places(vec![(two.clone(), 0, 0), (two.clone(), 0, 0)]);
    assert_eq!(ed.list_history.len(), 2);
    assert_eq!(ed.list_index, 2, "on the newest");
    // `:copen` then `:colder` shows the older one, in a window of its own.
    ed.execute_cmdline("copen");
    assert_eq!(ed.list_targets.len(), 2);
    ed.execute_cmdline("colder");
    assert_eq!(ed.list_index, 1);
    assert_eq!(ed.list_targets.len(), 1, "the older list");
    assert_eq!(ed.list_targets[0].0, one);
    assert_eq!(ed.windows.len(), 2, "one window, not two");
    assert!(ed.message.contains("1 of 2"), "{}", ed.message);
    // And back the other way.
    ed.execute_cmdline("cnewer");
    assert_eq!(ed.list_index, 2);
    assert_eq!(ed.list_targets.len(), 2);
    assert!(ed.message.contains("2 of 2"), "{}", ed.message);
    // The ends say so rather than wrapping.
    ed.execute_cmdline("cnewer");
    assert!(ed.message.contains("E553"), "{}", ed.message);
    assert_eq!(ed.list_index, 2);
    ed.execute_cmdline("colder");
    ed.execute_cmdline("colder");
    assert!(ed.message.contains("E553"), "{}", ed.message);
    assert_eq!(ed.list_index, 1);
    // The list the walk restored is the one `:cnext` walks.
    assert_eq!(ed.places.len(), 1);
}

#[test]
fn closing_a_list_window_takes_its_document_with_it() {
    let dir = std::env::temp_dir().join("inkstone-nolist");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("one.txt");
    std::fs::write(&one, "a\n").unwrap();

    let mut ed = Editor::for_tests();
    // A real file, opened the way a person opens one: poking at the live
    // buffer would leave the *document slot* empty and the name would be
    // `[No Name]`, which is the line this project keeps meeting.
    ed.open_file(&one).unwrap();
    let documents = ed.documents.len();
    ed.set_places(vec![(one.clone(), 0, 0)]);
    // Opening and closing a list ten times leaves nothing behind.
    for _ in 0..10 {
        ed.execute_cmdline("copen");
        assert_eq!(ed.documents.len(), documents + 1, "the list is a document");
        assert_eq!(ed.windows.len(), 2);
        assert_eq!(ed.current_document(), documents, "and it is on screen");
        ed.execute_cmdline("cclose");
        assert_eq!(ed.windows.len(), 1);
        assert_eq!(
            ed.documents.len(),
            documents,
            "and closing the window took it away"
        );
    }
    // The window that is left still shows the file it did, and the file is
    // where it was.
    assert_eq!(ed.buffer.display_name(), "one.txt");
    assert!(!ed.is_dirty(), "and the file is untouched");
    // Closing a window on a *file* still just closes the window.
    ed.split_window(Axis::Rows, None);
    assert_eq!(ed.windows.len(), 2);
    ed.execute_cmdline("cclose");
    assert!(ed.message.contains("E776"), "{}", ed.message);
    assert_eq!(ed.documents.len(), documents, "the file stayed open");
    assert_eq!(ed.buffer.display_name(), "one.txt");
}

#[test]
fn gf_opens_the_file_named_under_the_cursor() {
    let dir = std::env::temp_dir().join("inkstone-gf");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("one.txt"), "the one\n").unwrap();
    std::fs::write(dir.join("src").join("two.txt"), "the two\n").unwrap();
    let here = dir.join("here.txt");
    std::fs::write(&here, "see src/two.txt and one.txt\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.open_file(&here).unwrap();
    // The name is a *path*, not a word: `src/two.txt` is three words and one
    // file name, and `gf` takes the whole of it.
    ed.cursor.row = 0;
    ed.cursor.col = 6;
    feed(&mut ed, "gf");
    assert_eq!(ed.buffer.display_name(), "two.txt");
    assert_eq!(
        ed.buffer.text(),
        "the two\n",
        "and it is the file that opened"
    );
    assert!(ed.message.contains("two.txt"), "{}", ed.message);

    // Relative to the file that is open, which is what a name in a file
    // means. Back to it, and the plain name in the same line.
    ed.open_file(&here).unwrap();
    ed.cursor.row = 0;
    ed.cursor.col = 22;
    feed(&mut ed, "gf");
    assert_eq!(ed.buffer.display_name(), "one.txt");
    assert_eq!(ed.buffer.text(), "the one\n");

    // A name that is not there says so, and changes nothing.
    ed.open_file(&here).unwrap();
    let before = ed.buffer.display_name();
    ed.buffer = Buffer::from_text("see nowhere.txt\n");
    ed.cursor.row = 0;
    ed.cursor.col = 6;
    feed(&mut ed, "gf");
    assert!(
        ed.message.starts_with("E484"),
        "a name that is not there is named back: {}",
        ed.message
    );
    assert_eq!(ed.buffer.text(), "see nowhere.txt\n", "and nothing opened");
    let _ = before;

    // `'path'` is where a name is looked for when it is not beside the file:
    // the setting is a comma-separated list, searched in order, which is how a
    // name in an `import` line finds a file in another directory.
    let elsewhere = dir.join("lib");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("helper.txt"), "the helper\n").unwrap();
    let mut ed = Editor::for_tests();
    ed.open_file(&here).unwrap();
    ed.buffer = Buffer::from_text("see helper.txt\n");
    ed.cursor.row = 0;
    ed.cursor.col = 6;
    feed(&mut ed, "gf");
    assert!(
        ed.message.starts_with("E484"),
        "not found yet: {}",
        ed.message
    );
    ed.execute_cmdline(&format!("set path={}", elsewhere.display()));
    assert_eq!(ed.options.borrow().path.0.len(), 1, "one directory");
    ed.buffer = Buffer::from_text("see helper.txt\n");
    ed.cursor.row = 0;
    ed.cursor.col = 6;
    feed(&mut ed, "gf");
    assert_eq!(ed.buffer.display_name(), "helper.txt");
    assert_eq!(ed.buffer.text(), "the helper\n");
    // A list is written with commas, and the empty parts are not directories.
    ed.execute_cmdline("set path=/one,,/two");
    assert_eq!(
        ed.options.borrow().path.0.len(),
        2,
        "two directories, and no empty one"
    );
    assert_eq!(ed.options.borrow().path.0[1].display().to_string(), "/two");

    // Punctuation with no name in it is not a file name.
    ed.buffer = Buffer::from_text("...\n");
    ed.cursor.row = 0;
    ed.cursor.col = 1;
    feed(&mut ed, "gf");
    assert!(ed.message.starts_with("E484"), "{}", ed.message);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn listing_and_naming_documents() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("buffers-list-one.txt");
    let two = dir.join("buffers-list-two.txt");
    std::fs::write(&one, "one\ntwo\n").unwrap();
    std::fs::write(&two, "other\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&one).unwrap();
    feed(&mut ed, "j");
    ed.execute_cmdline(&format!("e {}", two.display()));

    ed.execute_cmdline("ls");
    assert!(
        ed.message.contains("1  h \"buffers-list-one.txt\" line 2"),
        "{}",
        ed.message
    );
    assert!(
        ed.message.contains("2 %a \"buffers-list-two.txt\" line 1"),
        "{}",
        ed.message
    );

    // A number, a full path and the end of one all find their document.
    ed.execute_cmdline("b 1");
    assert_eq!(ed.buffer.text(), "one\ntwo\n");
    ed.execute_cmdline("b 2");
    assert_eq!(ed.buffer.text(), "other\n");
    ed.execute_cmdline(&format!("b {}", one.display()));
    assert_eq!(ed.buffer.text(), "one\ntwo\n");
    ed.execute_cmdline("b two.txt");
    assert_eq!(ed.buffer.text(), "other\n");
    ed.execute_cmdline("b missing.txt");
    assert_eq!(ed.message, "E94: No matching buffer for missing.txt");
    ed.execute_cmdline("b");
    assert_eq!(ed.message, "E471: Argument required");
}

#[test]
fn a_path_entry_can_hold_a_wildcard() {
    let dir = std::env::temp_dir().join("inkstone-path");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/deep/inside")).unwrap();
    std::fs::create_dir_all(dir.join("other")).unwrap();
    std::fs::write(dir.join("top.rs"), "top\n").unwrap();
    std::fs::write(dir.join("src/one.rs"), "one\n").unwrap();
    std::fs::write(dir.join("src/deep/two.rs"), "two\n").unwrap();
    std::fs::write(dir.join("other/three.rs"), "three\n").unwrap();

    let mut ed = Editor::for_tests();
    // A buffer with no file of its own: every candidate comes from `'path'`.
    ed.buffer = Buffer::from_text("one.rs\n");
    ed.cursor = Cursor::new(0, 0);

    // `*` is a run of characters within one name.
    ed.execute_cmdline(&format!("set path={}", dir.join("src").display()));
    feed(&mut ed, "gf");
    assert!(
        ed.buffer
            .path()
            .is_some_and(|path| path.ends_with("one.rs")),
        "found in the plain entry: {:?}",
        ed.buffer.path()
    );

    // `**` is this directory and everything under it, which is how a name in an
    // `import` line finds a file in a tree.
    ed.buffer = Buffer::from_text("two.rs\n");
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline(&format!("set path={}/**", dir.display()));
    feed(&mut ed, "gf");
    assert!(
        ed.buffer
            .path()
            .is_some_and(|path| path.ends_with("two.rs")),
        "found two levels down: {:?}",
        ed.buffer.path()
    );

    // `**1` is one level, so the file two levels down is out of reach — and the
    // message says so rather than opening the wrong thing.
    ed.buffer = Buffer::from_text("two.rs\n");
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline(&format!("set path={}/**1", dir.display()));
    feed(&mut ed, "gf");
    assert_eq!(ed.message, "E484: Can't open file two.rs");
}

#[test]
fn gf_takes_a_count_and_gf_takes_a_line_number() {
    let dir = std::env::temp_dir().join("inkstone-gf-count");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("one")).unwrap();
    std::fs::create_dir_all(dir.join("two")).unwrap();
    std::fs::write(dir.join("one/same.rs"), "first\n").unwrap();
    std::fs::write(dir.join("two/same.rs"), "second\nthird\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("same.rs\n");
    ed.cursor = Cursor::new(0, 0);
    // The entries are searched in order, so the count picks which one.
    ed.execute_cmdline(&format!(
        "set path={},{}",
        dir.join("one").display(),
        dir.join("two").display()
    ));
    feed(&mut ed, "gf");
    assert_eq!(ed.buffer.text(), "first\n", "the first one found");
    ed.execute_cmdline("e #");
    ed.buffer = Buffer::from_text("same.rs\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "2gf");
    assert_eq!(ed.buffer.text(), "second\nthird\n", "the second one found");

    // `gF` takes the number after the name as a line in the file.
    ed.buffer = Buffer::from_text("same.rs 2\n");
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline(&format!("set path={}", dir.join("two").display()));
    feed(&mut ed, "gF");
    assert_eq!(ed.buffer.text(), "second\nthird\n");
    assert_eq!(ed.cursor.row, 1, "on the line the number named");

    // Trailing punctuation is not part of the name: a name in a sentence.
    ed.buffer = Buffer::from_text("see same.rs.\n");
    ed.cursor = Cursor::new(0, 6);
    feed(&mut ed, "gf");
    assert_eq!(
        ed.buffer.text(),
        "second\nthird\n",
        "the full stop is the sentence's"
    );
}

#[test]
fn a_path_entry_can_search_upwards() {
    // "include;/usr" searches `include`, then the directory it is in, and so on
    // up to the stop directory — which is how a name in an `#include` line finds
    // a header in a tree above it.
    let dir = std::env::temp_dir().join("inkstone-path-up");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("project/src")).unwrap();
    std::fs::write(dir.join("shared.h"), "// shared\n").unwrap();
    std::fs::write(dir.join("project/local.h"), "// local\n").unwrap();
    std::fs::write(dir.join("project/src/main.c"), "x\n").unwrap();

    let mut ed = Editor::for_tests();
    // `local.h` is not in `project/src`, but it is in the directory above it,
    // and the entry says to keep looking up to `project`.
    ed.buffer = Buffer::from_text("local.h\n");
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline(&format!(
        "set path={};{}",
        dir.join("project/src").display(),
        dir.join("project").display()
    ));
    feed(&mut ed, "gf");
    assert!(
        ed.buffer
            .path()
            .is_some_and(|path| path.ends_with("local.h")),
        "found in the directory above: {:?}",
        ed.buffer.path()
    );

    // With no stop directory it goes to the root, which is what finds a file two
    // levels up.
    ed.buffer = Buffer::from_text("shared.h\n");
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline(&format!("set path={};", dir.join("project/src").display()));
    feed(&mut ed, "gf");
    assert!(
        ed.buffer
            .path()
            .is_some_and(|path| path.ends_with("shared.h")),
        "found two levels up: {:?}",
        ed.buffer.path()
    );

    // A *relative* entry is walked the same way: `.` has no parent of its own, so
    // the walk has to start from what it resolves to. This is the case a smoke
    // test found and the absolute-path tests above did not.
    let here = std::env::current_dir().expect("a working directory");
    let _ = std::fs::write(here.join("inkstone-relative.h"), "// relative\n");
    ed.buffer = Buffer::from_text("inkstone-relative.h\n");
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline(&format!("set path=./src;{}", here.display()));
    feed(&mut ed, "gf");
    assert!(
        ed.buffer
            .path()
            .is_some_and(|path| path.ends_with("inkstone-relative.h")),
        "found by walking up from a relative entry: {:?}",
        ed.buffer.path()
    );
    let _ = std::fs::remove_file(here.join("inkstone-relative.h"));

    // A directory that is not there ends the walk rather than searching above it.
    ed.buffer = Buffer::from_text("shared.h\n");
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline(&format!(
        "set path={};{}",
        dir.join("nowhere/src").display(),
        dir.display()
    ));
    feed(&mut ed, "gf");
    assert_eq!(ed.message, "E484: Can't open file shared.h");
}
