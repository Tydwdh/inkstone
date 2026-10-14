use super::*;

#[test]
fn q_colon_shows_the_history_and_runs_a_line_of_it() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    // Nothing typed yet, so there is nothing to show. (The keys are fed one
    // at a time: the notation writes a two-key sequence with a space, and
    // `parse_key` reads a space as part of it rather than as a separator.)
    feed(&mut ed, "q");
    feed(&mut ed, ":");
    assert!(ed.message.contains("E776"), "{}", ed.message);
    assert_eq!(ed.windows.len(), 1, "and no window was opened");

    // Two commands *typed*, which is what the history is of — a command run
    // from a script is not something a person typed, and does not go in it.
    let prompt = |ed: &mut Editor, opens: &str, line: &str| {
        feed(ed, opens);
        feed(ed, line);
        feed(ed, "<CR>");
    };
    prompt(&mut ed, ":", "set number");
    prompt(&mut ed, ":", "set nonumber");
    feed(&mut ed, "q");
    feed(&mut ed, ":");
    assert_eq!(ed.list, Some(List::Commands), "it is a list");
    assert_eq!(ed.windows.len(), 2);
    let text = ed.buffer.text();
    assert!(text.contains("set number\n"), "{text:?}");
    assert!(text.contains("set nonumber\n"), "{text:?}");
    assert!(text.find("set number").unwrap() < text.find("set nonumber").unwrap());
    // `<CR>` on a line runs it again, and the window goes away — so what the
    // command did is not half a screen down.
    ed.cursor.row = 0;
    ed.open_place_at_cursor();
    assert_eq!(ed.windows.len(), 1, "the window closed");
    assert!(ed.options.borrow().number, "and the line ran");

    // `q/` is the same for searches, which are typed on the same line.
    prompt(&mut ed, "/", "one");
    assert_eq!(
        ed.search_history,
        vec!["one".to_string()],
        "and it was typed"
    );
    feed(&mut ed, "q");
    feed(&mut ed, "/");
    assert_eq!(ed.list, Some(List::Searches));
    let text = ed.buffer.text();
    assert!(text.contains("one"), "{text:?}");
    ed.cursor.row = 0;
    ed.open_place_at_cursor();
    assert_eq!(ed.windows.len(), 1);
    // (The message row is the name of the document the window went back to —
    // closing a window says what is on screen, which is what `:e` does too.)
    assert_eq!(ed.search.pattern(), "one", "the search ran");
    assert_eq!(ed.cursor.row, 0, "and it went to the match");
}

#[test]
fn the_prompt_is_shown_where_the_command_line_is() {
    let mut ed = Editor::for_tests();
    ed.handle_resize(20, 4);
    feed(&mut ed, "/t");
    assert_eq!(
        ed.compose().grid.row_text(3).trim_end(),
        "/t",
        "the bottom row shows the prompt and what is typed"
    );
    feed(&mut ed, "<Esc>");
    assert!(ed.compose().grid.row_text(3).trim_end().is_empty());

    // A `:` command still spells itself with a colon.
    feed(&mut ed, ":w");
    assert_eq!(ed.compose().grid.row_text(3).trim_end(), ":w");
}

#[test]
fn a_line_can_be_edited_wherever_the_cursor_is() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abc\n");
    feed(&mut ed, "/abc");
    feed(&mut ed, "<Home>x");
    assert_eq!(ed.cmdline, "xabc", "a character goes where the cursor is");

    feed(&mut ed, "<End><BS>");
    assert_eq!(ed.cmdline, "xab");
    feed(&mut ed, "<Left>");
    assert_eq!(ed.cmdline_cursor, 2);
    feed(&mut ed, "<Del>");
    assert_eq!(
        ed.cmdline, "xa",
        "`<Del>` takes the character under the cursor"
    );
    feed(&mut ed, "<BS>");
    assert_eq!(ed.cmdline, "x", "and `<BS>` the one before it");
    feed(&mut ed, "<Home><Del>");
    assert_eq!(ed.cmdline, "");

    feed(&mut ed, "abc<Enter>");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "and the edited pattern runs");
    assert_eq!(ed.message, "");
}

#[test]
fn up_and_down_walk_the_history_and_bring_back_the_draft() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, ":nope<Enter>");
    feed(&mut ed, ":also-nope<Enter>");
    assert_eq!(ed.command_history, vec!["nope", "also-nope"]);

    feed(&mut ed, ":half");
    feed(&mut ed, "<Up>");
    assert_eq!(ed.cmdline, "also-nope");
    feed(&mut ed, "<Up>");
    assert_eq!(ed.cmdline, "nope");
    feed(&mut ed, "<Up>");
    assert_eq!(ed.cmdline, "nope", "the oldest line is as far as it goes");
    feed(&mut ed, "<Down>");
    assert_eq!(ed.cmdline, "also-nope");
    feed(&mut ed, "<Down>");
    assert_eq!(ed.cmdline, "half", "and the line being typed comes back");
    feed(&mut ed, "<Down>");
    assert_eq!(ed.cmdline, "half", "past it, it stays");
}

#[test]
fn a_line_run_twice_is_remembered_once() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, ":nope<Enter>");
    feed(&mut ed, ":nope<Enter>");
    assert_eq!(ed.command_history, vec!["nope"]);
    feed(&mut ed, ":<Enter>");
    assert_eq!(
        ed.command_history,
        vec!["nope"],
        "an empty line is not kept"
    );
}

#[test]
fn escape_forgets_the_prompt_and_what_was_typed() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, ":nope<Enter>");
    feed(&mut ed, ":half<Up>");
    assert_eq!(ed.cmdline, "nope");
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.cmdline, "");
    assert_eq!(ed.cmdline_cursor, 0);
    assert_eq!(ed.prompt_index, None);
    assert_eq!(ed.prompt_draft, "");
    assert_eq!(ed.command_history, vec!["nope"], "the history is kept");
}

#[test]
fn each_document_keeps_its_own_undo_history() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("buffers-undo-one.txt");
    let two = dir.join("buffers-undo-two.txt");
    std::fs::write(&one, "one\n").unwrap();
    std::fs::write(&two, "other\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&one).unwrap();
    feed(&mut ed, "ix<Esc>"); // one -> xone
                              // `:e!` because the document that is open has changes; those changes
                              // stay with it rather than being thrown away.
    ed.execute_cmdline(&format!("e! {}", two.display()));
    feed(&mut ed, "ohello<Esc>");

    // Undoing in the second document must not touch the first, and must not
    // replay the first document's steps onto it: those hold indices into
    // text this document has never held.
    feed(&mut ed, "u");
    assert_eq!(
        ed.buffer.text(),
        "other\n",
        "the second document's own step"
    );
    ed.execute_cmdline("bp");
    assert_eq!(ed.buffer.text(), "xone\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\n", "the first document's own step");
}

#[test]
fn a_save_keeps_the_history_but_a_reload_forgets_it() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("undo-save.txt");
    std::fs::write(&path, "one\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&path).unwrap();
    feed(&mut ed, "ix<Esc>");
    ed.execute_cmdline(&format!("w {}", path.display()));
    assert!(!ed.is_dirty());

    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\n", "writing cleared no history");
    assert!(ed.is_dirty(), "and undoing makes it modified again");

    ed.execute_cmdline(&format!("e! {}", path.display()));
    feed(&mut ed, "u");
    assert_eq!(
        ed.message, "Already at oldest change",
        "a reload starts a fresh history"
    );
}

#[test]
fn the_history_window_can_be_edited() {
    // vim's command-line window: "you can now use commands to move around and
    // edit the text in the window", and what is not run is "discarded when
    // closing the window". The *other* lists stay read-only — a list that can be
    // edited is a list that can lie about what it says.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    ed.viewport = crate::screen::Viewport::new(40, 12);
    // Two-key sequences are fed a key at a time: `q` waits for the key that says
    // which history is wanted.
    feed(&mut ed, ":set number<Enter>");
    feed(&mut ed, "q");
    feed(&mut ed, ":");
    assert_eq!(ed.list, Some(List::Commands), "the command window is up");
    let height = ed.window_rects().last().expect("a window").height;
    assert_eq!(height, 7, "'cmdwinheight' says how tall it is");

    // Editing a line in it works, and the line `<CR>` runs is the edited one.
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "A!");
    assert!(
        ed.buffer.text().starts_with("set number!"),
        "the line was edited: {}",
        ed.buffer.text()
    );
    feed(&mut ed, "<Enter>");
    assert_eq!(ed.list, None, "the window went away");
    assert_eq!(ed.mode, Mode::Normal);

    // A settings list is a report, and stays read-only.
    ed.execute_cmdline("set all");
    assert_eq!(ed.list, Some(List::Settings));
    let before = ed.buffer.text();
    feed(&mut ed, "x");
    assert_eq!(
        ed.message,
        "E21: Cannot make changes, 'readonly' option is set"
    );
    assert_eq!(ed.buffer.text(), before, "nothing changed");
    assert!(List::Commands.is_editable() && !List::Settings.is_editable());
}

#[test]
fn the_command_line_window_has_its_own_rules() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    ed.viewport = crate::screen::Viewport::new(40, 12);
    feed(&mut ed, ":set number<Enter>");
    feed(&mut ed, "q");
    feed(&mut ed, ":");
    assert_eq!(ed.list, Some(List::Commands));

    // No nesting: "once a command-line window is open it is not possible to open
    // another one".
    feed(&mut ed, "q");
    feed(&mut ed, "/");
    assert_eq!(ed.message, "E1292: Command-line window is already open");
    assert_eq!(ed.windows.len(), 2, "still just the one window");

    // `<C-c>` continues in Command-line mode with the line under the cursor, and
    // the window stays visible.
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "<C-c>");
    assert_eq!(ed.mode, Mode::Command, "typing again");
    assert_eq!(ed.cmdline, "set number");
    assert_eq!(ed.prompt, Prompt::Command);
    assert_eq!(ed.list, Some(List::Commands), "the window is still there");

    // `<Esc>` leaves that line alone and the window is still a window.
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.mode, Mode::Normal);
    assert_eq!(ed.list, Some(List::Commands));
}
