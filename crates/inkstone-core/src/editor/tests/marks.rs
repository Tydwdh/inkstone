use super::*;

#[test]
fn marks_are_put_down_and_gone_back_to() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("first line\n    indented line\nthird\n");
    // `m` takes the next letter, and the cursor keeps its place.
    ed.cursor = Cursor::new(1, 6);
    feed(&mut ed, "ma");
    assert_eq!(
        ed.marks.get('a'),
        Some(Cursor::new(1, 6)),
        "where it was put"
    );
    assert_eq!(ed.cursor, Cursor::new(1, 6));
    // `` `a `` goes back to the exact character, `'a` to the line's first
    // non-blank one.
    ed.cursor = Cursor::new(2, 0);
    feed(&mut ed, "`a");
    assert_eq!(ed.cursor, Cursor::new(1, 6));
    ed.cursor = Cursor::new(2, 0);
    feed(&mut ed, "'a");
    assert_eq!(ed.cursor, Cursor::new(1, 4), "first non-blank");
    // A mark is a motion: `d'a` takes whole lines, its own line included,
    // and `` d`a `` the characters up to the one the mark is on.
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
    ed.cursor = Cursor::new(2, 4);
    feed(&mut ed, "mz");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "d'z");
    assert_eq!(ed.buffer.text(), "four\n", "lines one to three");
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    ed.cursor = Cursor::new(2, 1);
    feed(&mut ed, "mz");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "d`z");
    assert_eq!(ed.buffer.text(), "hree\n", "up to the mark's character");
    // A mark nobody set says so, and the cursor stays.
    ed.buffer = Buffer::from_text("one\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "`q");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    assert_eq!(ed.message, "E20: Mark not set: q");
    // The keys wait for a letter, and anything else calls them off.
    feed(&mut ed, "m");
    assert!(ed.mark_prefix.is_some());
    feed(&mut ed, "1");
    assert!(ed.mark_prefix.is_none(), "a digit is not a letter");
    feed(&mut ed, "`");
    feed(&mut ed, "<Esc>");
    assert!(ed.mark_prefix.is_none());
    // `:marks` lists what there is.
    ed.execute_cmdline("marks");
    assert!(
        ed.message.starts_with("2 marks   a 2:7   z 3:2"),
        "{}",
        ed.message
    );
}

#[test]
fn a_mark_stays_with_its_document() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.cursor = Cursor::new(1, 0);
    feed(&mut ed, "ma");
    // Another document has none of its own, and coming back finds it.
    ed.save_document();
    ed.documents.push(Document::new(
        Buffer::from_text("elsewhere\n"),
        Language::Plain,
    ));
    ed.windows[ed.current_window].document = ed.documents.len() - 1;
    ed.load_document();
    assert_eq!(ed.marks.get('a'), None, "a mark belongs to its file");
    ed.show_document(0);
    assert_eq!(ed.marks.get('a'), Some(Cursor::new(1, 0)));
}

#[test]
fn a_range_can_name_a_mark() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\nfive\n");
    // `a` on line two, `b` on line four: `:'a,'b` is lines two to four.
    ed.cursor = Cursor::new(1, 0);
    feed(&mut ed, "ma");
    ed.cursor = Cursor::new(3, 0);
    feed(&mut ed, "mb");
    ed.execute_cmdline("'a,'bd");
    assert_eq!(ed.buffer.text(), "one\nfive\n", "the three lines between");
    // A substitute over the same range, and one address on its own, which
    // is that line and not the rest of the file.
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\nfive\n");
    ed.cursor = Cursor::new(1, 0);
    feed(&mut ed, "ma");
    ed.execute_cmdline("'a,$s/o/O/g");
    assert_eq!(
        ed.buffer.text(),
        "one\ntwO\nthree\nfOur\nfive\n",
        "from the mark to the end"
    );
    // The mark is on line two, and one address is that line: the `t` of
    // "three" is not touched.
    ed.execute_cmdline("'as/t/T/");
    assert_eq!(ed.buffer.text(), "one\nTwO\nthree\nfOur\nfive\n");
    // `:g` over a mark's range.
    ed.buffer = Buffer::from_text("one\ntwo TODO\nthree\nfour TODO\n");
    ed.cursor = Cursor::new(1, 0);
    feed(&mut ed, "ma");
    ed.execute_cmdline("'a,2g/TODO/d");
    assert_eq!(
        ed.buffer.text(),
        "one\nthree\nfour TODO\n",
        "only inside it"
    );
    // A mark nobody set stops the command rather than pointing at a line
    // nobody asked for.
    ed.execute_cmdline("'z,'zd");
    assert_eq!(
        ed.buffer.text(),
        "one\nthree\nfour TODO\n",
        "nothing changed"
    );
    assert_eq!(ed.message, "E20: Mark not set: z");
}

#[test]
fn the_last_jump_is_where_the_keys_came_from() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "mz");
    ed.cursor = Cursor::new(3, 2);
    feed(&mut ed, "`z");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "to the mark");
    // `''` comes back to where that jump started, and again to toggle.
    feed(&mut ed, "''");
    assert_eq!(
        ed.cursor,
        Cursor::new(3, 0),
        "back to its line, first non-blank"
    );
    feed(&mut ed, "''");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "and back again");
    // The exact one keeps the column, the line one goes to the first
    // non-blank.
    ed.buffer = Buffer::from_text("one\n    two\nthree\n");
    ed.cursor = Cursor::new(2, 1);
    feed(&mut ed, "ma");
    ed.cursor = Cursor::new(0, 2);
    feed(&mut ed, "`a");
    assert_eq!(ed.cursor, Cursor::new(2, 1));
    feed(&mut ed, "``");
    assert_eq!(ed.cursor, Cursor::new(0, 2), "the exact last jump");
    feed(&mut ed, "`a");
    feed(&mut ed, "''");
    assert_eq!(
        ed.cursor,
        Cursor::new(0, 0),
        "the first non-blank of its line"
    );
    // A search is a jump too.
    ed.buffer = Buffer::from_text("one two\none three\none four\n");
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline("s/one/x/");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "/three<CR>");
    assert_eq!(ed.cursor.row, 1);
    feed(&mut ed, "''");
    assert_eq!(ed.cursor.row, 0, "back to where the search started");
    // Nothing to go back to, and it says so.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "''");
    assert!(ed.message.contains("E20"), "{}", ed.message);
}

#[test]
fn the_jump_list_walks_back_and_forward() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\nfive\n");
    // Marks on three lines, and jumps between them: each jump leaves the
    // place it started in the list.
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "ma");
    ed.cursor = Cursor::new(2, 0);
    feed(&mut ed, "mb");
    ed.cursor = Cursor::new(4, 0);
    feed(&mut ed, "mc");
    ed.cursor = Cursor::new(4, 0);
    feed(&mut ed, "`a");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    feed(&mut ed, "`b");
    assert_eq!(ed.cursor, Cursor::new(2, 0));
    // Back: where the last jump started, then the one before it.
    feed(&mut ed, "<C-o>");
    assert_eq!(
        ed.cursor,
        Cursor::new(0, 0),
        "where the second jump started"
    );
    feed(&mut ed, "<C-o>");
    assert_eq!(ed.cursor, Cursor::new(4, 0), "and the first one");
    // Forward again, and no further than the end.
    feed(&mut ed, "<C-i>");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    feed(&mut ed, "<C-i>");
    assert_eq!(ed.cursor, Cursor::new(2, 0), "which is where we were");
    feed(&mut ed, "<C-i>");
    assert_eq!(ed.cursor, Cursor::new(2, 0), "the end of the list");
    assert!(ed.message.contains("E20"), "{}", ed.message);
    // `<Tab>` is the same key as `<C-i>`, which is what a terminal gives
    // for either of them.
    feed(&mut ed, "2<C-o>");
    assert_eq!(ed.cursor, Cursor::new(4, 0), "two back, in one go");
    feed(&mut ed, "<Tab>");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "one forward");
    feed(&mut ed, "2<C-o>");
    assert_eq!(ed.cursor, Cursor::new(4, 0), "back again");
    // `:jumps` lists them — newest first, with the walk's place marked.
    ed.execute_cmdline("jumps");
    assert!(ed.message.contains("jumps"), "{}", ed.message);
    assert!(ed.message.contains('>'), "{}", ed.message);
}

#[test]
fn marks_and_jumps_can_be_taken_away() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
    for (row, letter) in [(0usize, 'a'), (1, 'b'), (2, 'c'), (3, 'd')] {
        ed.cursor = Cursor::new(row, 0);
        feed(&mut ed, &format!("m{letter}"));
    }
    // One letter at a time, together or apart.
    ed.execute_cmdline("delmarks a");
    assert_eq!(ed.marks.get('a'), None);
    assert_eq!(ed.marks.get('b'), Some(Cursor::new(1, 0)), "only that one");
    assert_eq!(ed.message, "1 mark deleted");
    ed.execute_cmdline("delmarks b c");
    assert_eq!(ed.marks.iter().count(), 1, "`d` is what is left");
    // A run of them, both ends included — and they do not have to be there.
    ed.cursor = Cursor::new(0, 0);
    for letter in ['a', 'b', 'c'] {
        feed(&mut ed, &format!("m{letter}"));
    }
    ed.execute_cmdline("delmarks a-c");
    assert_eq!(ed.marks.get('a'), None);
    assert_eq!(ed.marks.get('b'), None);
    assert_eq!(ed.marks.get('c'), None);
    assert_eq!(ed.marks.get('d'), Some(Cursor::new(3, 0)), "untouched");
    assert_eq!(ed.message, "3 marks deleted");
    // Something that is not a letter is named back rather than guessed at.
    ed.execute_cmdline("delmarks A");
    assert!(ed.message.contains("E474"), "{}", ed.message);
    ed.execute_cmdline("delmarks a-");
    assert_eq!(ed.marks.iter().count(), 1, "a hyphen with nothing after it");
    // Everything at once.
    ed.execute_cmdline("delmarks!");
    assert!(ed.marks.is_empty());
    // And the jump list, which `:jumps` then finds nothing in.
    ed.execute_cmdline("mz");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "`z");
    feed(&mut ed, "<C-o>");
    assert!(!ed.jumps.is_empty());
    ed.execute_cmdline("clearjumps");
    assert!(ed.jumps.is_empty());
    ed.execute_cmdline("jumps");
    assert_eq!(ed.message, "No jumps");
    feed(&mut ed, "<C-o>");
    assert!(ed.message.contains("E20"), "{}", ed.message);
}

#[test]
fn the_change_list_walks_where_edits_were_made() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
    // Three edits, on three different lines.
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "x");
    ed.cursor = Cursor::new(2, 0);
    feed(&mut ed, "x");
    ed.cursor = Cursor::new(3, 0);
    feed(&mut ed, "x");
    assert_eq!(ed.buffer.text(), "ne\ntwo\nhree\nour\n");
    // `g;` walks back through them, `g,` forward again.
    feed(&mut ed, "g;");
    assert_eq!(ed.cursor.row, 2);
    feed(&mut ed, "g;");
    assert_eq!(ed.cursor.row, 0);
    feed(&mut ed, "g;");
    assert_eq!(ed.cursor.row, 0, "nothing older");
    assert!(ed.message.contains("no older change"), "{}", ed.message);
    feed(&mut ed, "g,");
    assert_eq!(ed.cursor.row, 2);
    feed(&mut ed, "2g,");
    assert_eq!(ed.cursor.row, 3, "a count walks that many at once");
    feed(&mut ed, "g,");
    assert!(ed.message.contains("no newer change"), "{}", ed.message);
    // `:changes` lists them with the line each edit is on now.
    ed.execute_cmdline("changes");
    assert!(ed.message.contains("3 changes"), "{}", ed.message);
    assert!(ed.message.contains("ne"), "{}", ed.message);
    // An insert session is one change on one line, however many keys it
    // took: the list is per line, not per keystroke.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("\n\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "ithree words<Esc>");
    assert_eq!(ed.buffer.text(), "three words\n\n");
    ed.execute_cmdline("changes");
    assert!(ed.message.starts_with("1 changes"), "{}", ed.message);
}

#[test]
fn a_wrapped_list_keeps_its_marker_on_one_line() {
    let mut ed = Editor::for_tests();
    ed.options.borrow_mut().textwidth = crate::settings::Count(20);
    // A bullet: the marker on the first line, spaces under the text after it.
    ed.buffer = Buffer::from_text("- one two three four\n- five six seven\n");
    feed(&mut ed, "gqG");
    assert_eq!(
        ed.buffer.text(),
        "- one two three four\n  five six seven\n",
        "the second line is lined up under the first's text"
    );
    // A number, which is wider. (One line, since `1. ` and `2. ` are not a
    // leader two lines share — the digits differ.)
    ed.buffer = Buffer::from_text("1. alpha beta gamma delta\n");
    feed(&mut ed, "gqG");
    assert_eq!(
        ed.buffer.text(),
        "1. alpha beta gamma\n   delta\n",
        "three spaces under the text"
    );
    // A quotation is not a list: its marker is repeated on every line, since
    // every line of a quotation is quoted.
    ed.buffer = Buffer::from_text("> one two three\n> four five six\n");
    feed(&mut ed, "gqG");
    assert_eq!(ed.buffer.text(), "> one two three four\n> five six\n");
    // And a leader that is only indentation is repeated — with the
    // indentation not counting towards the width, so sixteen columns of text
    // under four columns of indent.
    ed.buffer = Buffer::from_text("    one two three\n    four five\n");
    feed(&mut ed, "gqG");
    assert_eq!(
        ed.buffer.text(),
        "    one two three\n    four five\n",
        "the same lines, since sixteen columns is what fits"
    );
}

#[test]
fn a_macro_records_and_plays() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    ed.cursor.row = 0;
    // `qa` starts recording, `x` and `j` go into it, and `q` ends it.
    feed(&mut ed, "qa");
    assert_eq!(ed.message, "recording @a");
    feed(&mut ed, "xj");
    feed(&mut ed, "q");
    assert_eq!(ed.message, "recorded @a");
    assert_eq!(
        ed.buffer.text(),
        "ne\ntwo\nthree\n",
        "the keys did their work"
    );
    assert_eq!(ed.macros[0].as_deref(), Some("xj"), "and only those keys");
    assert!(ed.recording.is_none());
    // Playing it does the same again, from where the cursor is.
    feed(&mut ed, "@a");
    assert_eq!(ed.buffer.text(), "ne\nwo\nthree\n");
    assert_eq!(
        ed.cursor.row, 2,
        "and the cursor is where the macro left it"
    );
    // A count plays it that many times, and `@@` plays the last one again.
    feed(&mut ed, "2@a");
    assert_eq!(ed.buffer.text(), "ne\nwo\nree\n");
    feed(&mut ed, "@@");
    assert_eq!(ed.buffer.text(), "ne\nwo\nee\n");
    // A register nobody recorded into says so, and does nothing.
    let before = ed.buffer.text();
    feed(&mut ed, "@z");
    assert!(ed.message.contains("E748"), "{}", ed.message);
    assert_eq!(ed.buffer.text(), before);

    // `q:` is still the history window: the letter-taking state hands the
    // pair the keymap names back to it.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, ":");
    feed(&mut ed, "set number");
    feed(&mut ed, "<CR>");
    feed(&mut ed, "q");
    feed(&mut ed, ":");
    assert_eq!(ed.list, Some(List::Commands), "the window opened");
    assert!(ed.recording.is_none(), "and no recording was started");
}

#[test]
fn undoing_back_to_the_written_state_clears_the_marker() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("undo-mark.txt");
    std::fs::write(&path, "one\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&path).unwrap();
    assert!(
        !ed.is_dirty(),
        "a file that has just been read is not changed"
    );

    feed(&mut ed, "ix<Esc>");
    assert!(ed.is_dirty(), "an edit makes it changed");
    ed.execute_cmdline("w");
    assert!(!ed.is_dirty(), "and writing makes it changed no longer");

    feed(&mut ed, "x");
    assert!(ed.is_dirty());
    feed(&mut ed, "u");
    assert!(
        !ed.is_dirty(),
        "undoing back to the written state clears it"
    );
    feed(&mut ed, "u");
    assert!(ed.is_dirty(), "and undoing past it does not");
    feed(&mut ed, "<C-r>");
    assert!(!ed.is_dirty(), "redo takes it back to the written state");
    feed(&mut ed, "<C-r>");
    assert!(ed.is_dirty(), "and on past it");

    // `:q` believes it, which is the point of the whole thing.
    feed(&mut ed, "u");
    assert!(!ed.is_dirty());
    ed.execute_cmdline("q");
    assert!(
        ed.should_quit,
        "a file that is as it was written can be left"
    );
}
