use super::*;

#[test]
fn toggle_case_walks_over_what_it_changed() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("heLLo world\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "~");
    assert_eq!(ed.buffer.text(), "HeLLo world\n");
    assert_eq!(ed.cursor.col, 1, "and moves on");
    // A count, wiping out the case of a word — and back again.
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "5~");
    assert_eq!(ed.buffer.text(), "hEllO world\n", "five characters");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "5~");
    assert_eq!(ed.buffer.text(), "HeLLo world\n", "toggled, not set");
    // Non-letters are left alone, and the end of the buffer is the end.
    ed.buffer = Buffer::from_text("1a2b\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "99~");
    assert_eq!(ed.buffer.text(), "1A2B\n", "the terminator is not a letter");
    assert_eq!(ed.cursor.col, 3, "and the cursor stays on the line");
}

#[test]
fn the_choice_a_stop_offers_can_be_picked_from() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "i");
    ed.set_completions(vec![CompletionItem {
        label: "s".to_string(),
        insert: "let ${1|one,two,three|} = ${2:value};".to_string(),
        detail: String::new(),
        snippet: true,
    }]);
    // The first of the choices is what is typed, and accepting the item
    // puts the list away: what is left is a snippet on its own.
    assert_eq!(ed.buffer.line_string(0), "let one = value;");
    feed(&mut ed, "<C-y>");
    assert!(ed.completion.is_none(), "the list is away");
    // Into the first stop, and then `<C-n>`/`<C-p>` pick another, in place.
    feed(&mut ed, "<Tab>");
    feed(&mut ed, "<C-n>");
    assert_eq!(ed.buffer.line_string(0), "let two = value;");
    feed(&mut ed, "<C-n>");
    assert_eq!(ed.buffer.line_string(0), "let three = value;");
    feed(&mut ed, "<C-n>");
    assert_eq!(ed.buffer.line_string(0), "let one = value;", "round again");
    feed(&mut ed, "<C-p>");
    assert_eq!(ed.buffer.line_string(0), "let three = value;", "and back");
    // `<Tab>` still walks the stops, and the last one moved with the text:
    // `three` is longer than `one`, and `value` is still where it belongs.
    feed(&mut ed, "<Tab>");
    assert_eq!(ed.buffer.line_string(0), "let three = value;");
    let (from, to) = ed.selected_range().expect("the second stop is selected");
    assert_eq!(
        ed.buffer.slice_text(from, to),
        "value",
        "the second stop followed the first one's growth"
    );
    feed(&mut ed, "<Tab>");
    assert!(ed.snippet_stops.is_empty(), "and then the session is over");
    // A stop with nothing to choose from takes no key: the list's keys mean
    // what they always meant.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "i");
    ed.set_completions(vec![CompletionItem {
        label: "s".to_string(),
        insert: "plain ${1:word}".to_string(),
        detail: String::new(),
        snippet: true,
    }]);
    feed(&mut ed, "<Tab>");
    let before = ed.buffer.line_string(0);
    assert!(
        !ed.snippet_choose(true),
        "one option is not something to pick from"
    );
    assert_eq!(ed.buffer.line_string(0), before, "and the text stands");
}

#[test]
fn a_place_written_twice_says_what_the_first_one_says() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "i");
    ed.set_completions(vec![CompletionItem {
        label: "swap".to_string(),
        insert: "let ${1:name} = ${1:name};".to_string(),
        detail: String::new(),
        snippet: true,
    }]);
    assert_eq!(ed.buffer.line_string(0), "let name = name;");
    feed(&mut ed, "<C-y>");
    // The second writing is a mirror: the walk steps to the place once.
    assert_eq!(ed.snippet_stops.len(), 2);
    assert!(ed.snippet_stops[1].mirror, "the second is a mirror");
    feed(&mut ed, "<Tab>");
    assert_eq!(ed.cursor.char_idx(&ed.buffer), 7, "at the end of the place");
    assert!(ed.selection.is_some(), "with its text selected");
    // Typing replaces what is selected, and the mirror says the same.
    ed.type_text("value");
    assert_eq!(ed.buffer.line_string(0), "let value = value;");
    assert!(ed.selection.is_none(), "the selection went with it");
    // And the walk goes on from there rather than stopping at the mirror.
    feed(&mut ed, "<Tab>");
    assert!(ed.snippet_stops.is_empty(), "one place, and then over");
}

#[test]
fn typing_over_a_placeholder_replaces_it() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "i");
    ed.set_completions(vec![CompletionItem {
        label: "s".to_string(),
        insert: "let ${1:name} = 1;".to_string(),
        detail: String::new(),
        snippet: true,
    }]);
    feed(&mut ed, "<C-y>");
    assert_eq!(ed.buffer.line_string(0), "let name = 1;");
    // `<Tab>` selects the placeholder, and what is typed replaces it rather
    // than going in beside it.
    feed(&mut ed, "<Tab>");
    assert!(ed.selection.is_some(), "the placeholder is selected");
    ed.type_text("value");
    assert_eq!(ed.buffer.line_string(0), "let value = 1;");
    assert!(ed.selection.is_none(), "and the selection is spent");
    // One `u` takes back the whole insert session — the snippet that was
    // accepted and the typing over its placeholder are one visit to insert
    // mode, and a visit is one step. (The placeholder is not a step of its
    // own: it never existed outside the session.)
    feed(&mut ed, "<Esc>");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.line_string(0), "", "the session, all of it");
}

#[test]
fn lines_can_be_sorted() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("banana\napple\ncherry\n");
    ed.execute_cmdline("sort");
    assert_eq!(ed.buffer.text(), "apple\nbanana\ncherry\n");
    assert_eq!(ed.message, "3 lines sorted");
    // The other way round, and a range smaller than the file.
    ed.execute_cmdline("sort!");
    assert_eq!(ed.buffer.text(), "cherry\nbanana\napple\n");
    ed.execute_cmdline("2,3sort");
    assert_eq!(
        ed.buffer.text(),
        "cherry\napple\nbanana\n",
        "only two of them"
    );
    // One edit, however many lines moved: one `u` puts it all back.
    let before = ed.buffer.text();
    ed.execute_cmdline("sort");
    assert_ne!(ed.buffer.text(), before);
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), before);
    // Numbers compare as numbers: `9` before `10`.
    ed.buffer = Buffer::from_text("10 ten\n9 nine\n2 two\n");
    ed.execute_cmdline("sort n");
    assert_eq!(ed.buffer.text(), "2 two\n9 nine\n10 ten\n");
    // Without `n` they compare as text, which is the other order.
    ed.execute_cmdline("sort!");
    ed.execute_cmdline("sort");
    assert_eq!(ed.buffer.text(), "10 ten\n2 two\n9 nine\n");
    // Case can be ignored, and one of each kept.
    ed.buffer = Buffer::from_text("Beta\nalpha\nALPHA\nbeta\n");
    ed.execute_cmdline("sort iu");
    assert_eq!(ed.buffer.text(), "alpha\nBeta\n", "no case, no repeats");
    // A flag vim does not have is named back rather than ignored.
    ed.execute_cmdline("sort z");
    assert!(ed.message.contains("E474"), "{}", ed.message);
    // Sorting does not change how the file ends: the terminators are the
    // ones already in the range, so a file that ends without a line break
    // still does.
    ed.buffer = Buffer::from_text("b\na\nz");
    let newlines = ed.buffer.text().matches('\n').count();
    let ended = ed.buffer.text().ends_with('\n');
    ed.execute_cmdline("sort");
    assert_eq!(ed.buffer.text().matches('\n').count(), newlines);
    assert_eq!(ed.buffer.text().ends_with('\n'), ended);
    assert!(ed.buffer.text().starts_with("a\nb\nz"));
    // And a one-line file is a file nothing happens to.
    ed.buffer = Buffer::from_text("only\n");
    ed.execute_cmdline("sort");
    assert_eq!(ed.buffer.text(), "only\n");
    assert_eq!(ed.message, "1 line sorted");
}

#[test]
fn keys_can_be_run_over_a_range_of_lines() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    // The old idiom: append a semicolon to every line.
    ed.execute_cmdline("%normal A;");
    assert_eq!(ed.buffer.text(), "one;\ntwo;\nthree;\n");
    assert_eq!(ed.message, "3 lines normalised");
    // A range, and a key that is not a letter.
    ed.execute_cmdline("2,3normal ^x");
    assert_eq!(
        ed.buffer.text(),
        "one;\nwo;\nhree;\n",
        "two of them, one key each"
    );
    // `<Esc>` is the Escape key here, in the notation everything else uses.
    ed.execute_cmdline("%normal 0i> <Esc>A <");
    assert_eq!(ed.buffer.text(), "> one; <\n> wo; <\n> hree; <\n");
    // One line is one line, and no line is no line.
    ed.buffer = Buffer::from_text("only\n");
    ed.execute_cmdline("normal x");
    assert_eq!(ed.buffer.text(), "nly\n");
    assert_eq!(ed.message, "1 line normalised");
    // A name that is not a key is the characters it looks like, rather than
    // an error: a person writing `<` meant `<`.
    ed.buffer = Buffer::from_text("ab\n");
    ed.execute_cmdline("%normal i<notakey><Esc>0");
    assert_eq!(ed.buffer.text(), "<notakey>ab\n");
    // Spaces in the keys are keys, not separators.
    ed.buffer = Buffer::from_text("ab\n");
    ed.execute_cmdline("%normal i x");
    assert_eq!(ed.buffer.text(), " xab\n", "the space was typed");
}

#[test]
fn a_file_can_be_read_below_a_line_and_part_of_one_written_out() {
    let dir = std::env::temp_dir().join("inkstone-read-write");
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.txt");
    std::fs::write(&source, "alpha\nbeta\n").unwrap();
    let target = dir.join("target.txt");
    let _ = std::fs::remove_file(&target);

    // `:read` puts the file's lines below the cursor's own, as lines.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline(&format!("read {}", source.display()));
    assert_eq!(ed.buffer.text(), "one\nalpha\nbeta\ntwo\n");
    assert!(ed.message.starts_with("2 lines read"), "{}", ed.message);
    // Read at the very end, where the last line has no terminator to sit
    // after: the file still comes in as its own lines.
    ed.buffer = Buffer::from_text("only");
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline(&format!("read {}", source.display()));
    assert_eq!(ed.buffer.text(), "only\nalpha\nbeta\n");
    // A file that is not there says so rather than reading nothing.
    ed.execute_cmdline(&format!("read {}", dir.join("nope.txt").display()));
    assert_eq!(ed.buffer.text(), "only\nalpha\nbeta\n");
    assert!(!ed.message.is_empty(), "{}", ed.message);

    // `:{range}write {file}` writes those lines and no others.
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
    ed.execute_cmdline(&format!("2,3w {}", target.display()));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "two\nthree\n");
    assert!(ed.message.starts_with("2 lines written"), "{}", ed.message);
    // Writing part of a buffer does not make the whole of it saved.
    assert!(ed.is_dirty(), "the rest is still unsaved");
    // One line, and no argument at all.
    ed.execute_cmdline(&format!("1w {}", target.display()));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "one\n");
    ed.execute_cmdline("2w");
    assert!(ed.message.contains("E471"), "{}", ed.message);
    // `:w >> file` adds to what is there instead of replacing it.
    std::fs::write(&target, "already\n").unwrap();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.execute_cmdline(&format!("w >> {}", target.display()));
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "already\none\ntwo\n",
        "and the whole file when no range was named"
    );
    ed.execute_cmdline(&format!("1w >> {}", target.display()));
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "already\none\ntwo\none\n",
        "or the part of it a range named"
    );
    assert!(ed.message.starts_with("1 line appended"), "{}", ed.message);
    // `:{range}read` puts the lines after that line rather than the
    // cursor's.
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    ed.cursor = Cursor::new(0, 0);
    std::fs::write(&target, "read-me\n").unwrap();
    ed.execute_cmdline(&format!("2read {}", target.display()));
    assert_eq!(ed.buffer.text(), "one\ntwo\nread-me\nthree\n");
    // `:w` with no range is still the whole file — and `:w!` is what it
    // takes, since this test wrote that file itself and the buffer that is
    // saving has never read it.
    ed.buffer = Buffer::from_text("whole\n");
    ed.execute_cmdline(&format!("w {}", target.display()));
    assert!(
        ed.message.contains("E13"),
        "a file that changed on disk is not overwritten without asking: {}",
        ed.message
    );
    ed.execute_cmdline(&format!("w! {}", target.display()));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "whole\n");
    assert!(!ed.is_dirty(), "and that *is* the buffer being saved");
}

#[test]
fn the_places_a_question_named_can_be_walked() {
    let dir = std::env::temp_dir().join("inkstone-places");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("one.txt");
    let two = dir.join("two.txt");
    std::fs::write(&one, "first\nsecond\n").unwrap();
    std::fs::write(&two, "third\nfourth\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("mine\n");
    // With nothing asked, there is nothing to walk.
    ed.execute_cmdline("cnext");
    assert!(ed.message.contains("E776"), "{}", ed.message);
    // A question fills the list, and the walk starts at the first place.
    ed.set_places(vec![
        (one.clone(), 0, 0),
        (one.clone(), 1, 2),
        (two.clone(), 1, 0),
    ]);
    assert_eq!(ed.place_index, 1);
    ed.execute_cmdline("clist");
    assert!(ed.message.starts_with("3 places"), "{}", ed.message);
    assert!(ed.message.contains(">1 one.txt:1"), "{}", ed.message);
    // `:cnext` walks forwards and goes to the place it reaches — opening the
    // file if it is another one.
    ed.execute_cmdline("cnext");
    assert_eq!(ed.place_index, 2);
    assert_eq!(ed.cursor.row, 1);
    assert_eq!(ed.cursor.col, 2);
    ed.execute_cmdline("cnext");
    assert_eq!(ed.place_index, 3);
    assert!(
        ed.buffer.path().is_some_and(|path| path == two),
        "the third place is in the other file: {:?}",
        ed.buffer.path()
    );
    assert_eq!(ed.cursor.row, 1);
    // Off the end it stops and says so; `:cprev` walks back.
    ed.execute_cmdline("cnext");
    assert!(ed.message.contains("E553"), "{}", ed.message);
    assert_eq!(ed.place_index, 3, "and stays where it was");
    ed.execute_cmdline("cprev");
    assert_eq!(ed.place_index, 2);
    assert!(ed.buffer.path().is_some_and(|path| path == one));
    // A count walks that many at once, and off the front it stops too.
    ed.execute_cmdline("cprev");
    ed.execute_cmdline("2cprev");
    assert!(ed.message.contains("E553"), "{}", ed.message);
    assert_eq!(ed.place_index, 1, "still the first");
}

#[test]
fn a_setting_can_be_set_from_the_colon_line() {
    let mut ed = Editor::for_tests();
    ed.execute_cmdline("set textwidth=20");
    assert_eq!(ed.options.borrow().textwidth.0, 20);
    assert_eq!(ed.message, "textwidth=20");
    // A number has no "on", so a bare name asks — and `?` asks for both
    // kinds, which is what a vim-shaped editor owes anyone who types it.
    ed.execute_cmdline("set textwidth");
    assert_eq!(ed.message, "textwidth=20");
    assert_eq!(ed.options.borrow().textwidth.0, 20, "and changes nothing");
    ed.execute_cmdline("set textwidth?");
    assert_eq!(ed.message, "textwidth=20");
    ed.execute_cmdline("set number?");
    assert_eq!(ed.message, "number=false", "the switch is off");
    // A switch that is named alone is turned *on*, which is what `:set
    // number` means everywhere else.
    ed.execute_cmdline("set number");
    assert!(ed.options.borrow().number, "{}", ed.message);
    assert_eq!(ed.message, "number=true");
    ed.execute_cmdline("set nonumber");
    assert!(!ed.options.borrow().number);
    assert_eq!(ed.message, "number=false");
    ed.execute_cmdline("set nope?");
    assert!(ed.message.contains("E518"), "{}", ed.message);
    // The values a script writes are the values here.
    ed.execute_cmdline("set number=true");
    assert!(ed.options.borrow().number);
    ed.execute_cmdline("set number=false");
    assert!(!ed.options.borrow().number);
    // A number that does not fit the kind of setting says so.
    ed.execute_cmdline("set textwidth=-1");
    assert!(ed.message.starts_with("E474"), "{}", ed.message);
    assert_eq!(ed.options.borrow().textwidth.0, 20, "and leaves it alone");
    ed.execute_cmdline("set number=perhaps");
    assert!(ed.message.starts_with("E474"), "{}", ed.message);
    // And an option nobody has is named back.
    ed.execute_cmdline("set nope=1");
    assert!(ed.message.contains("E518"), "{}", ed.message);
}

#[test]
fn gq_keeps_what_every_line_had_in_front_of_it() {
    let mut ed = Editor::for_tests();
    ed.options.borrow_mut().textwidth = crate::settings::Count(24);
    ed.buffer = Buffer::from_text("// one two three\n// four five six\n");
    feed(&mut ed, "gqG");
    assert_eq!(
        ed.buffer.text(),
        "// one two three four\n// five six\n",
        "the marker stays on both lines"
    );
    // A quotation, which has the same shape.
    ed.buffer = Buffer::from_text("> alpha beta gamma\n> delta epsilon\n");
    feed(&mut ed, "gqG");
    assert_eq!(ed.buffer.text(), "> alpha beta gamma delta\n> epsilon\n");
    // Indentation is a leader too, and lines that disagree have none.
    ed.buffer = Buffer::from_text("    one two three four five\n    six seven\n");
    feed(&mut ed, "gqG");
    assert_eq!(
        ed.buffer.text(),
        "    one two three four\n    five six seven\n"
    );
    ed.buffer = Buffer::from_text("one two three four five six\n  seven eight\n");
    feed(&mut ed, "gqG");
    assert_eq!(
        ed.buffer.text(),
        "one two three four five\nsix seven eight\n",
        "nothing shared, nothing kept"
    );
}

#[test]
fn a_line_breaks_while_it_is_being_typed() {
    let mut ed = Editor::for_tests();
    ed.options.borrow_mut().textwidth = crate::settings::Count(12);
    ed.buffer = Buffer::from_text("\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "i");
    for c in "one two three four".chars() {
        ed.type_text(&c.to_string());
    }
    assert_eq!(
        ed.buffer.text(),
        "one two\nthree four\n",
        "broken at the last space before the width"
    );
    // A word longer than the width is not broken, because there is nowhere
    // to break it.
    let mut ed = Editor::for_tests();
    ed.options.borrow_mut().textwidth = crate::settings::Count(6);
    ed.buffer = Buffer::from_text("\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "i");
    ed.type_text("abcdefghij");
    assert_eq!(ed.buffer.text(), "abcdefghij\n", "no space, no break");
    // And zero is still "do not wrap".
    let mut ed = Editor::for_tests();
    ed.options.borrow_mut().textwidth = crate::settings::Count(0);
    ed.buffer = Buffer::from_text("\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "i");
    ed.type_text("one two three four five six");
    assert_eq!(ed.buffer.text(), "one two three four five six\n");
}

#[test]
fn a_boolean_setting_can_be_turned_off_by_name() {
    let mut ed = Editor::for_tests();
    ed.options.borrow_mut().number = true;
    ed.execute_cmdline("set nonumber");
    assert!(!ed.options.borrow().number);
    assert_eq!(ed.message, "number=false");
    // `no` on something that is not a switch is not a name at all.
    ed.execute_cmdline("set notabstop");
    assert!(ed.message.contains("E518"), "{}", ed.message);
    assert_eq!(ed.options.borrow().tabstop, 4, "and nothing changed");
}

#[test]
fn a_new_line_starts_where_the_last_one_did() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("    one two\nthree\n");
    // Off by default, as in vim.
    ed.cursor = Cursor::new(0, 8);
    feed(&mut ed, "o");
    assert_eq!(ed.buffer.text(), "    one two\n\nthree\n");
    assert_eq!(ed.cursor, Cursor::new(1, 0), "at the start, not indented");
    feed(&mut ed, "<Esc>");
    ed.buffer = Buffer::from_text("    one two\nthree\n");
    ed.cursor = Cursor::new(0, 8);
    ed.options.borrow_mut().autoindent = true;
    assert!(ed.options.borrow().autoindent, "the setting is on");
    // `o` puts the new line below, with the same indentation, and the cursor
    // after it.
    assert_eq!(ed.indent_of(0), "    ", "the indent is read");
    feed(&mut ed, "o");
    assert_eq!(ed.buffer.text(), "    one two\n    \nthree\n");
    assert_eq!(ed.cursor, Cursor::new(1, 4), "after the indentation");
    ed.type_text("more");
    assert_eq!(ed.buffer.text(), "    one two\n    more\nthree\n");
    feed(&mut ed, "<Esc>");
    // `O` puts it above, with the indentation of the line it was opened
    // from — which is the line the cursor is on, as in vim.
    ed.buffer = Buffer::from_text("    one two\nthree\n");
    ed.cursor = Cursor::new(0, 8);
    feed(&mut ed, "O");
    assert_eq!(ed.buffer.text(), "    \n    one two\nthree\n");
    assert_eq!(ed.cursor, Cursor::new(0, 4));
    feed(&mut ed, "<Esc>");
    // A line break typed in insert mode does it too, as one edit.
    ed.buffer = Buffer::from_text("    one\n");
    ed.cursor = Cursor::new(0, 7);
    feed(&mut ed, "a<CR>");
    assert_eq!(ed.buffer.text(), "    one\n    \n");
    assert_eq!(ed.cursor, Cursor::new(1, 4));
    feed(&mut ed, "<Esc>");
    feed(&mut ed, "u");
    assert_eq!(
        ed.buffer.text(),
        "    one\n",
        "the break and the indent together"
    );
    // A line with no indentation has none to copy.
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.cursor = Cursor::new(0, 2);
    feed(&mut ed, "o");
    assert_eq!(ed.buffer.text(), "one\n\ntwo\n");
    assert_eq!(ed.cursor, Cursor::new(1, 0));
}

#[test]
fn an_operator_can_be_forced_to_characters_or_lines() {
    // Vim's `o_v`: after an operator, `v` forces the motion to work
    // characterwise — a linewise motion becomes *exclusive*, and a
    // characterwise one has its inclusive/exclusive the other way round.
    const SOURCE: &str = "one two three\nfour five\n";
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(SOURCE);
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    // `dj` would take two whole lines; `dvj` takes from the cursor to the
    // character below it, and not that character.
    feed(&mut ed, "dvj");
    assert_eq!(
        ed.buffer.text(),
        "one  five\n",
        "from the cursor up to — but not including — the character below it"
    );

    // `$` is inclusive, so `v` makes it exclusive: the last character stays.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(SOURCE);
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "dv$");
    assert_eq!(
        ed.buffer.text(),
        "one e\nfour five\n",
        "everything but the last character"
    );

    // `w` is exclusive, so `v` makes it inclusive: the next word's first
    // character goes too.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(SOURCE);
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "dvw");
    assert_eq!(
        ed.buffer.text(),
        "one hree\nfour five\n",
        "one character further"
    );

    // `o_V` is the other way: `V` forces whole lines, whatever the motion
    // was. `dVw` takes the whole line the motion reaches.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(SOURCE);
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "dVw");
    assert_eq!(ed.buffer.text(), "four five\n", "the whole line");

    // And the shape is spent by the motion it was given to: the next operator
    // is its own again.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(SOURCE);
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "dvjd");
    assert_eq!(ed.forced_shape, None, "nothing is left forced");
    assert_eq!(
        ed.buffer.text(),
        "one  five\n",
        "and `d` is linewise again — `dj` takes what is left of two lines"
    );
}

#[test]
fn j_joins_lines() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n    two\nthree\n");
    ed.cursor.row = 0;
    // `J` joins the cursor's line with the next, taking the next one's indent
    // out and putting a space in its place.
    feed(&mut ed, "J");
    assert_eq!(ed.buffer.text(), "one two\nthree\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\n    two\nthree\n", "one `u`");

    // A count joins that many lines, and one `u` takes the whole thing back.
    feed(&mut ed, "3J");
    assert_eq!(ed.buffer.text(), "one two three\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\n    two\nthree\n");

    // `'joinspaces'` is on, so a line that ends a sentence is joined with two
    // spaces — and with one when the option is off.
    let dir = std::env::temp_dir().join("inkstone-joinspaces");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sentences.txt");
    std::fs::write(&path, "One.\n    Two\nThree!\nFour\n").unwrap();
    let mut ed = Editor::for_tests();
    ed.open_file(&path).unwrap();
    assert!(ed.options.borrow().joinspaces, "on by default, as in vim");
    feed(&mut ed, "4J");
    assert_eq!(
        ed.buffer.text(),
        "One.  Two Three!  Four\n",
        "two spaces after each full stop, one after the other line"
    );
    feed(&mut ed, "u");
    assert_eq!(
        ed.buffer.text(),
        "One.\n    Two\nThree!\nFour\n",
        "one `u` takes the whole join back"
    );
    ed.execute_cmdline("set nojoinspaces");
    feed(&mut ed, "4J");
    assert_eq!(ed.buffer.text(), "One. Two Three! Four\n", "one space each");
    let _ = std::fs::remove_dir_all(&dir);

    // `gJ` inserts and removes no spaces at all: the indent of the next line
    // stays exactly where it was.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("One.\n    Two\nThree!\n");
    feed(&mut ed, "gJ");
    assert_eq!(ed.buffer.text(), "One.    Two\nThree!\n", "no space added");
    feed(&mut ed, "u");
    feed(&mut ed, "g3J");
    assert_eq!(ed.buffer.text(), "One.    TwoThree!\n", "and none anywhere");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "One.\n    Two\nThree!\n", "one `u`");

    // The last line has nothing below it to join to.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.cursor.row = 1;
    feed(&mut ed, "J");
    assert!(ed.message.starts_with("E85"), "{}", ed.message);
    assert_eq!(ed.buffer.text(), "one\ntwo\n", "and nothing moved");

    // `vJ` joins every line of the selection, which is what vim's "always whole
    // lines" note means.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n    two\n        three\nfour\n");
    ed.cursor.row = 0;
    feed(&mut ed, "vjj");
    feed(&mut ed, "J");
    assert_eq!(ed.buffer.text(), "one two three\nfour\n");
    assert_eq!(ed.mode, Mode::Normal, "and visual mode is over");
    feed(&mut ed, "u");
    assert_eq!(
        ed.buffer.text(),
        "one\n    two\n        three\nfour\n",
        "one `u` for the whole join"
    );
}

#[test]
fn delete_char_and_delete_line() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abc\ndef\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "x");
    assert_eq!(ed.buffer.text(), "bc\ndef\n");
    feed(&mut ed, "dd");
    assert_eq!(ed.buffer.text(), "def\n");
    feed(&mut ed, "dd");
    assert_eq!(ed.buffer.text(), "\n");
    feed(&mut ed, "dd");
    assert_eq!(
        ed.buffer.text(),
        "\n",
        "the last line cannot be removed entirely"
    );
}

#[test]
fn a_yanked_line_goes_below_or_above() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "yy");
    assert_eq!(ed.buffer.text(), "one\ntwo\n", "yanking changes nothing");
    feed(&mut ed, "p");
    assert_eq!(ed.buffer.text(), "one\none\ntwo\n", "below the line");
    feed(&mut ed, "P");
    assert_eq!(ed.buffer.text(), "one\none\none\ntwo\n", "above it");
    // The last line is the interesting one: there is no line below it.
    feed(&mut ed, "Gyy");
    feed(&mut ed, "p");
    assert_eq!(ed.buffer.text(), "one\none\none\ntwo\ntwo\n");
}

#[test]
fn a_yanked_line_keeps_its_own_terminator() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\r\ntwo\r\n");
    feed(&mut ed, "yyp");
    assert_eq!(
        ed.buffer.text(),
        "one\r\none\r\ntwo\r\n",
        "a CRLF line stays a CRLF line"
    );
}

#[test]
fn change_takes_the_word_and_leaves_the_space() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "cw");
    assert_eq!(ed.mode, Mode::Insert);
    assert_eq!(ed.buffer.text(), " two\n", "`cw` is `ce` in disguise");
    feed(&mut ed, "1<Esc>");
    assert_eq!(ed.buffer.text(), "1 two\n");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "cc");
    assert_eq!(ed.mode, Mode::Insert);
    assert_eq!(
        ed.buffer.text(),
        "\ntwo\n",
        "`cc` leaves the line behind, empty — it is not `dd` then `i`"
    );
    feed(&mut ed, "x<Esc>");
    assert_eq!(ed.buffer.text(), "x\ntwo\n");

    // The same for a linewise change that takes more than one line.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    feed(&mut ed, "cj");
    assert_eq!(ed.buffer.text(), "\nthree\n", "one line where two were");
    assert_eq!(ed.mode, Mode::Insert);
}

#[test]
fn text_objects_name_the_thing_the_operator_runs_over() {
    // `diw` on the middle word.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("alpha bravo charlie\n");
    feed(&mut ed, "wdiw");
    assert_eq!(
        ed.buffer.text(),
        "alpha  charlie\n",
        "the word, not the space"
    );
    assert_eq!(ed.cursor.col, 6, "and the cursor stays where it started");

    // `daw` takes the space after it, so no double space is left behind.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("alpha bravo charlie\n");
    feed(&mut ed, "wdaw");
    assert_eq!(ed.buffer.text(), "alpha charlie\n");

    // ...and at the end of a line it takes the space *before* it instead.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("alpha bravo\n");
    feed(&mut ed, "wdaw");
    assert_eq!(ed.buffer.text(), "alpha\n");

    // `ciw` changes the word and leaves insert mode waiting.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("alpha bravo\n");
    feed(&mut ed, "wciwtwo<Esc>");
    assert_eq!(ed.buffer.text(), "alpha two\n");

    // `yiw` copies the word without changing anything.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("alpha bravo\n");
    feed(&mut ed, "wyiw$p");
    assert_eq!(
        ed.buffer.text(),
        "alpha bravobravo\n",
        "the word was yanked"
    );
}

#[test]
fn debug_ciw() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("alpha bravo\n");
    feed(&mut ed, "w");
    println!("after w: cursor={:?}", ed.cursor);
    feed(&mut ed, "ciw");
    println!(
        "after ciw: text={:?} cursor={:?} mode={:?}",
        ed.buffer.text(),
        ed.cursor,
        ed.mode
    );
    feed(&mut ed, "two<Esc>");
    println!("after two: text={:?}", ed.buffer.text());
}

#[test]
fn text_objects_for_quotes_and_brackets() {
    // `ci"` on the text between quotes.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("let name = \"old\";\n");
    feed(&mut ed, "ci\"new<Esc>");
    assert_eq!(ed.buffer.text(), "let name = \"new\";\n");

    // `da"` takes the quotes with it.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("let name = \"old\";\n");
    feed(&mut ed, "da\"");
    assert_eq!(ed.buffer.text(), "let name = ;\n");

    // `di(` from anywhere inside the call, including on the opening bracket.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("call(one, two)\n");
    feed(&mut ed, "wdi(");
    assert_eq!(ed.buffer.text(), "call()\n");

    // A block's brackets are a pair across lines.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("fn main() {\n    body();\n}\n");
    feed(&mut ed, "jdi{");
    assert_eq!(
        ed.buffer.text(),
        "fn main() {\n}\n",
        "the body, across lines"
    );

    // And the whole block, brackets included.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("fn main() {\n    body();\n}\n");
    feed(&mut ed, "jda{");
    assert_eq!(ed.buffer.text(), "fn main() \n");

    // `viw` selects it, and `d` takes the selection.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("alpha bravo\n");
    feed(&mut ed, "wviwd");
    assert_eq!(ed.buffer.text(), "alpha \n");
    assert_eq!(ed.mode, Mode::Normal);
}

#[test]
fn a_double_click_selects_the_word_under_the_pointer() {
    use inkstone_rpc::{PointerKind, PointerMsg};
    let mut ed = Editor::for_tests();
    ed.viewport.width = 40;
    ed.viewport.height = 12;
    ed.buffer = Buffer::from_text("one two three\n");
    ed.options.borrow_mut().number = true;
    let gutter = crate::render::gutter_width(ed.buffer.line_count(), true);
    // The click lands in the middle of `two`, and the selection is the whole
    // word either way — the manual's "double → word".
    let at = gutter + 5;
    ed.handle_pointer(&PointerMsg::click(PointerKind::Press, at, 0, 2));
    assert_eq!(ed.mode, Mode::Visual, "a double click selects");
    let selected = |ed: &Editor| {
        let (start, end) = ed.visual_span().expect("a selection");
        ed.buffer.slice_text(start, end)
    };
    assert_eq!(selected(&ed), "two");
    assert_eq!(ed.cursor, Cursor::new(0, 6), "the cursor is at its end");
    // The button comes up, which is the second half of the second click: the
    // selection stays, or a double click would be undone the moment the
    // button was released.
    ed.handle_pointer(&PointerMsg::new(PointerKind::Release, at, 0));
    assert_eq!(selected(&ed), "two", "still selected after the release");
    assert_eq!(ed.mode, Mode::Visual);
    // A click in the space between words takes that space: there is no word
    // there, and a selection has to be something.
    ed.handle_pointer(&PointerMsg::click(PointerKind::Press, gutter + 3, 0, 2));
    assert_eq!(selected(&ed), " ", "the space under the pointer");

    // A single click is still a click: nothing stays selected.
    ed.handle_pointer(&PointerMsg::new(PointerKind::Press, at, 0));
    assert_eq!(ed.mode, Mode::Visual);
    ed.handle_pointer(&PointerMsg::new(PointerKind::Release, at, 0));
    assert!(ed.selection.is_none(), "a press and a release is a click");
    assert_eq!(ed.mode, Mode::Normal);
}

#[test]
fn a_double_click_on_a_bracket_selects_up_to_its_pair() {
    use inkstone_rpc::{PointerKind, PointerMsg};
    let mut ed = Editor::for_tests();
    ed.viewport.width = 40;
    ed.viewport.height = 12;
    ed.buffer = Buffer::from_text("f(a b) x\n");
    let at = |col: u16| col;
    // On the `(`: the manual's "a double click on a character that has a
    // match selects until that match (like using `v%`)".
    ed.handle_pointer(&PointerMsg::click(PointerKind::Press, at(1), 0, 2));
    let (start, end) = ed.visual_span().expect("a selection");
    assert_eq!(ed.buffer.slice_text(start, end), "(a b)");
    assert_eq!(ed.cursor, Cursor::new(0, 5), "the cursor is on the `)`");
    // On the word inside them: the word, not the brackets — the character
    // clicked is the one asked about.
    ed.handle_pointer(&PointerMsg::click(PointerKind::Press, at(2), 0, 2));
    let (start, end) = ed.visual_span().expect("a selection");
    assert_eq!(ed.buffer.slice_text(start, end), "a");
}

#[test]
fn a_drag_after_a_double_click_takes_whole_words() {
    use inkstone_rpc::{PointerKind, PointerMsg};
    let mut ed = Editor::for_tests();
    ed.viewport.width = 40;
    ed.viewport.height = 12;
    ed.buffer = Buffer::from_text("one two three\n");
    // "Dragging the mouse with a double click … will result in whole words
    // to be selected." The drag ends in the middle of `three`, and the
    // selection still covers it whole.
    ed.handle_pointer(&PointerMsg::click(PointerKind::Press, 1, 0, 2));
    ed.handle_pointer(&PointerMsg::new(PointerKind::Drag, 10, 0));
    let (start, end) = ed.visual_span().expect("a selection");
    assert_eq!(ed.buffer.slice_text(start, end), "one two three");
    // Backwards, the same: whole words — from the start of the word dragged
    // onto, to the end of the one that was double clicked.
    ed.handle_pointer(&PointerMsg::click(PointerKind::Press, 10, 0, 2));
    ed.handle_pointer(&PointerMsg::new(PointerKind::Drag, 1, 0));
    let (start, end) = ed.visual_span().expect("a selection");
    assert_eq!(ed.buffer.slice_text(start, end), "one two three");
    // Releasing ends the gesture, so a later drag is per character again.
    ed.handle_pointer(&PointerMsg::new(PointerKind::Release, 1, 0));
    ed.handle_pointer(&PointerMsg::new(PointerKind::Press, 1, 0));
    ed.handle_pointer(&PointerMsg::new(PointerKind::Drag, 2, 0));
    let (start, end) = ed.visual_span().expect("a selection");
    assert_eq!(
        ed.buffer.slice_text(start, end),
        "ne",
        "columns one to two, per character again"
    );
}

#[test]
fn a_dragged_separator_moves_the_window_beside_it() {
    use inkstone_rpc::{PointerKind, PointerMsg};
    let pointer = |kind: PointerKind, x: u16, y: u16| PointerMsg::new(kind, x, y);

    let mut ed = Editor::for_tests();
    ed.viewport.width = 40;
    ed.viewport.height = 12;
    ed.split_window(Axis::Columns, None);
    let rects = ed.window_rects();
    let (left, right) = (rects[0], rects[1]);
    assert_eq!(right.x, left.width + 1, "a column between them");
    let before = left.width;

    // A press on the separator starts a resize, not a selection.
    ed.handle_pointer(&pointer(PointerKind::Press, right.x - 1, 0));
    assert!(ed.resizing.is_some(), "the separator was picked up");
    assert!(ed.selection.is_none(), "and nothing was selected");
    assert_eq!(ed.mode, Mode::Normal);

    // Dragging it three columns to the left takes three from the left
    // window, and the right one takes them.
    ed.handle_pointer(&pointer(PointerKind::Drag, right.x - 4, 0));
    let rects = ed.window_rects();
    assert_eq!(rects[0].width, before - 3);
    assert_eq!(
        rects[0].width + rects[1].width + 1,
        40,
        "still the whole width"
    );
    // Dragging back is the same distance the other way, from the size the
    // drag started at rather than from the last event.
    ed.handle_pointer(&pointer(PointerKind::Drag, right.x - 1, 0));
    assert_eq!(ed.window_rects()[0].width, before, "back where it started");
    ed.handle_pointer(&pointer(PointerKind::Drag, right.x, 0));
    assert_eq!(
        ed.window_rects()[0].width,
        before + 1,
        "one column on, not two"
    );

    // It cannot be dragged off the screen: a window has to show something.
    ed.handle_pointer(&pointer(PointerKind::Drag, 0, 0));
    assert!(ed.window_rects()[0].width >= 1);
    assert_eq!(
        ed.window_rects()[0].width + ed.window_rects()[1].width + 1,
        40
    );
    ed.handle_pointer(&pointer(PointerKind::Release, 0, 0));
    assert!(ed.resizing.is_none(), "the drag is over");

    // A press on text is still a selection.
    let mut ed = Editor::for_tests();
    ed.viewport.width = 40;
    ed.viewport.height = 12;
    ed.buffer = Buffer::from_text("alpha\nbeta\n");
    ed.split_window(Axis::Columns, None);
    ed.handle_pointer(&pointer(PointerKind::Press, 0, 0));
    assert!(ed.resizing.is_none());
    assert_eq!(ed.mode, Mode::Visual, "text is selected, as it always was");

    // And the row between two stacked windows: the upper one's status row.
    let mut ed = Editor::for_tests();
    ed.viewport.width = 40;
    ed.viewport.height = 12;
    ed.split_window(Axis::Rows, None);
    let rects = ed.window_rects();
    let (upper, lower) = (rects[0], rects[1]);
    assert_eq!(lower.y, upper.y + upper.height, "they tile the space");
    ed.handle_pointer(&pointer(PointerKind::Press, 1, upper.y + upper.height - 1));
    assert!(ed.resizing.is_some(), "the row between them is a separator");
    ed.handle_pointer(&pointer(
        PointerKind::Drag,
        1,
        upper.y + upper.height - 1 + 2,
    ));
    assert_eq!(ed.window_rects()[0].height, upper.height + 2);
    assert_eq!(
        ed.window_rects()[0].height + ed.window_rects()[1].height,
        11
    );
}

#[test]
fn a_split_can_be_resized() {
    let mut ed = Editor::for_tests();
    ed.viewport.width = 80;
    ed.viewport.height = 24;
    ed.split_window(Axis::Rows, None);
    assert_eq!(ed.windows.len(), 2);
    let current = |ed: &Editor| ed.window_rects()[ed.current_window];
    let other = |ed: &Editor| ed.window_rects()[1 - ed.current_window];
    assert_eq!(
        current(&ed).height + other(&ed).height,
        23,
        "the whole height"
    );
    let equal = current(&ed).height;

    // `:resize 5` is five rows of text: the window keeps a row of its own
    // for its status line, so it is six screen rows.
    ed.execute_cmdline("resize 5");
    assert_eq!(current(&ed).height, 6);
    assert_eq!(
        current(&ed).height + other(&ed).height,
        23,
        "still the whole height"
    );
    let (a, b) = (current(&ed), other(&ed));
    let (above, below) = if a.y <= b.y { (a, b) } else { (b, a) };
    assert_eq!(below.y, above.y + above.height, "they tile the space");

    // A bare `:resize` grows by one; `-2` takes two away and `+3` adds three.
    ed.execute_cmdline("resize");
    assert_eq!(current(&ed).height, 7);
    ed.execute_cmdline("resize -2");
    assert_eq!(current(&ed).height, 5);
    ed.execute_cmdline("resize +3");
    assert_eq!(current(&ed).height, 8);

    // `<C-w>=` brings the equal shares back.
    feed(&mut ed, "<C-w>=");
    assert_eq!(current(&ed).height, equal, "as it was");
    ed.execute_cmdline("resize 9");
    assert_eq!(current(&ed).height, 10);
    feed(&mut ed, "<C-w>=");
    assert_eq!(current(&ed).height, equal, "equal again");
    assert_eq!(current(&ed).height + other(&ed).height, 23);

    // Across: `:vertical resize` is the same arithmetic on widths.
    let mut ed = Editor::for_tests();
    ed.viewport.width = 80;
    ed.viewport.height = 24;
    ed.split_window(Axis::Columns, None);
    ed.execute_cmdline("vertical resize 30");
    let rects = ed.window_rects();
    assert_eq!(rects[1].width, 30, "the window that is up");
    assert_eq!(rects[1].x, rects[0].width + 1, "a column between them");
    assert_eq!(
        rects[0].width + rects[1].width + 1,
        80,
        "and the whole width"
    );

    // A window with no division beside it is the whole rectangle: there is
    // nothing to resize and nothing to break.
    let mut ed = Editor::for_tests();
    ed.viewport.width = 80;
    ed.viewport.height = 24;
    ed.execute_cmdline("resize 5");
    assert_eq!(ed.message, "E36: Not enough room");
    assert_eq!(ed.window_rects()[0].height, 23);

    // A size that is not a number says so.
    let mut ed = Editor::for_tests();
    ed.viewport.height = 24;
    ed.split_window(Axis::Rows, None);
    ed.execute_cmdline("resize wide");
    assert_eq!(ed.message, "E475: Invalid argument: wide");
}

#[test]
fn tabs_keep_their_own_windows_and_share_the_documents() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("tabs-one.txt");
    let two = dir.join("tabs-two.txt");
    std::fs::write(&one, "one\ntwo\n").unwrap();
    std::fs::write(&two, "other\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&one).unwrap();
    ed.viewport.width = 40;
    ed.viewport.height = 12;
    ed.execute_cmdline("vs"); // two windows in the first tab
    assert_eq!(ed.windows.len(), 2);

    // `:tab split` puts the same document in a tab of its own, with one
    // window, and the first tab keeps its two.
    ed.execute_cmdline("tab split");
    assert_eq!(ed.tabs.len(), 2);
    assert_eq!(ed.current_tab, 1);
    assert_eq!(ed.windows.len(), 1, "a new tab starts with one window");
    assert_eq!(ed.buffer.text(), "one\ntwo\n", "the same document");
    assert!(ed.status().ends_with("[2/2]"), "{}", ed.status());

    ed.execute_cmdline("tabprevious");
    assert_eq!(ed.current_tab, 0);
    assert_eq!(ed.windows.len(), 2, "the first tab's windows are its own");
    ed.execute_cmdline("tabnext");
    assert_eq!(ed.current_tab, 1);
    assert_eq!(ed.windows.len(), 1);
    ed.execute_cmdline("tabnext");
    assert_eq!(ed.current_tab, 0, "and they wrap around");

    // The documents are everybody's: opened in one tab, reachable in another.
    ed.execute_cmdline(&format!("e {}", two.display()));
    assert_eq!(ed.documents.len(), 2);
    ed.execute_cmdline("tabnext");
    ed.execute_cmdline("b 1");
    assert_eq!(
        ed.buffer.text(),
        "one\ntwo\n",
        "the first document, from tab 2"
    );
    ed.execute_cmdline("ls");
    assert!(ed.message.contains("tabs-two.txt"), "{}", ed.message);

    // `:tabnew` is a tab on an empty file of its own.
    ed.execute_cmdline("tabnew");
    assert_eq!(ed.tabs.len(), 3);
    assert_eq!(ed.buffer.text(), "\n");
    assert_eq!(ed.buffer.path(), None);

    // Closing tabs, and the last one refusing to go.
    ed.execute_cmdline("tabclose");
    assert_eq!(ed.tabs.len(), 2);
    ed.execute_cmdline("tabclose");
    assert_eq!(ed.tabs.len(), 1);
    assert!(!ed.status().contains('['), "{}", ed.status());
    ed.execute_cmdline("tabclose");
    assert_eq!(ed.tabs.len(), 1, "the last tab stays");
    assert_eq!(ed.message, "E784: Cannot close last tab page");
}

#[test]
fn paragraph_objects_take_the_lines_around_the_cursor() {
    let text = "one\ntwo\n\nthree\nfour\n\n\nfive\n";

    // `dip` takes the paragraph's lines, and nothing of the blank ones.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(text);
    feed(&mut ed, "dip");
    assert_eq!(ed.buffer.text(), "\nthree\nfour\n\n\nfive\n");
    assert!(ed.register.is_linewise(), "a paragraph is whole lines");
    // And what it took pastes as lines, the way `dd` does.
    feed(&mut ed, "p");
    assert_eq!(ed.buffer.text(), "\none\ntwo\nthree\nfour\n\n\nfive\n");

    // From inside a paragraph, the whole paragraph goes.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(text);
    feed(&mut ed, "jjjdip");
    assert_eq!(ed.buffer.text(), "one\ntwo\n\n\n\nfive\n");

    // `dap` takes the blank lines after it as well.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(text);
    feed(&mut ed, "dap");
    assert_eq!(ed.buffer.text(), "three\nfour\n\n\nfive\n");

    // ...and the ones before it when the paragraph ends the buffer.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(text);
    feed(&mut ed, "Gdap");
    assert_eq!(ed.buffer.text(), "one\ntwo\n\nthree\nfour\n");

    // On a blank line, `ip` is the blank lines themselves: the run of them
    // the cursor is in, which is what clearing the gap between two
    // paragraphs means.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(text);
    feed(&mut ed, "Gkkdip");
    assert_eq!(ed.buffer.text(), "one\ntwo\n\nthree\nfour\nfive\n");

    // `yip` and the visual form.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(text);
    feed(&mut ed, "yip");
    assert_eq!(ed.register.text(), "one\ntwo\n");
    assert_eq!(ed.buffer.text(), text, "a yank changes nothing");
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(text);
    feed(&mut ed, "vipd");
    assert_eq!(ed.buffer.text(), "\nthree\nfour\n\n\nfive\n");
}

#[test]
fn an_unknown_text_object_leaves_the_editor_alone() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("alpha bravo\n");
    feed(&mut ed, "diz");
    assert_eq!(ed.buffer.text(), "alpha bravo\n", "nothing was deleted");
    assert_eq!(ed.message, "z is not a text object");
    assert_eq!(ed.mode, Mode::Normal);
    assert!(ed.operator.is_none(), "and the operator is spent");
}

#[test]
fn the_first_file_takes_the_empty_window_s_place() {
    // `inkstone file` leaves one document behind, not an empty one and a
    // file — the first file a person opens *is* what they are editing.
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("buffers-first.txt");
    std::fs::write(&path, "one\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("e {}", path.display()));
    assert_eq!(ed.documents.len(), 1);
    assert_eq!(ed.buffer.text(), "one\n");

    // But once something has been typed it is a document like any other.
    let mut ed = Editor::for_tests();
    feed(&mut ed, "ikept<Esc>");
    let other = dir.join("buffers-first-other.txt");
    std::fs::write(&other, "two\n").unwrap();
    ed.execute_cmdline(&format!("e! {}", other.display()));
    assert_eq!(ed.documents.len(), 2, "the typed-in window is kept");
}

#[test]
fn a_second_file_arrives_as_a_document_of_its_own() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("buffers-one.txt");
    let two = dir.join("buffers-two.txt");
    std::fs::write(&one, "one\ntwo\nthree\n").unwrap();
    std::fs::write(&two, "other\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&one).unwrap();
    feed(&mut ed, "jj");
    assert_eq!(ed.cursor.row, 2);

    // `:e` on a *different* file keeps the one that was open.
    ed.execute_cmdline(&format!("e {}", two.display()));
    assert_eq!(ed.documents.len(), 2, "the first document is still open");
    assert_eq!(ed.buffer.text(), "other\n");
    assert_eq!(ed.cursor.row, 0, "a new document starts at the top");
    assert_eq!(ed.message, "\"buffers-two.txt\"");

    // Each document keeps its own cursor.
    ed.execute_cmdline("bp");
    assert_eq!(ed.buffer.text(), "one\ntwo\nthree\n");
    assert_eq!(ed.cursor.row, 2, "the cursor came back with its document");
    ed.execute_cmdline("bn");
    assert_eq!(ed.buffer.text(), "other\n");
    assert_eq!(ed.cursor.row, 0);
}

#[test]
fn the_window_keeps_its_size_and_the_document_its_place() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("buffers-view-one.rs");
    let two = dir.join("buffers-view-two.rs");
    std::fs::write(&one, "one\ntwo\nthree\n").unwrap();
    std::fs::write(&two, "fn main() {}\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&one).unwrap();
    ed.viewport.width = 80;
    ed.viewport.height = 10;
    feed(&mut ed, "jj");
    assert_eq!(ed.cursor.row, 2);

    // A file that arrives is looked at from the top, and shows that file's
    // colours: the document is new, the window is the same one.
    ed.execute_cmdline(&format!("e {}", two.display()));
    assert_eq!(ed.cursor.row, 0, "a new file starts at the top");
    assert_eq!(ed.viewport.top_line, 0);
    assert_eq!(ed.viewport.height, 10, "the size is the window's");

    // Coming back to a document brings its cursor with it, and leaves the
    // scroll alone: the scroll belongs to the window that is looking.
    ed.viewport.top_line = 5;
    ed.execute_cmdline("bp");
    assert_eq!(ed.cursor.row, 2, "the document's own place");
    assert_eq!(ed.viewport.top_line, 5, "the window's own scroll");
    ed.execute_cmdline("bn");
    assert_eq!(ed.cursor.row, 0, "and back again");
}

#[test]
fn two_windows_on_one_document_keep_their_own_positions() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("buffers-split.txt");
    std::fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&path).unwrap();
    ed.viewport.width = 80;
    ed.viewport.height = 12;
    feed(&mut ed, "jj");

    // :vs makes a second window beside this one, on the same document and
    // at the same place.
    ed.execute_cmdline("vs");
    assert_eq!(ed.windows.len(), 2, "a window beside another");
    assert_eq!(ed.documents.len(), 1, "on the same document");
    assert_eq!(ed.current_window, 1, "and vim's habit: the new one is up");
    assert_eq!(ed.cursor.row, 2, "the new window copies the position");

    // They move apart, and each keeps what it did.
    feed(&mut ed, "j");
    assert_eq!(ed.cursor.row, 3);
    feed(&mut ed, "<C-w>h");
    assert_eq!(ed.current_window, 0);
    assert_eq!(ed.cursor.row, 2, "the first window is where it was");
    ed.viewport.top_line = 2;
    feed(&mut ed, "<C-w>l");
    assert_eq!(ed.current_window, 1);
    assert_eq!(ed.cursor.row, 3);
    assert_ne!(ed.viewport.top_line, 2, "each window has its own view");
    feed(&mut ed, "<C-w>h");
    assert_eq!(ed.viewport.top_line, 2, "and the first kept its own");

    // Both look at one document, so an edit in one is in the other — and
    // the other does not move.
    feed(&mut ed, "<C-w>l");
    feed(&mut ed, "x");
    assert_eq!(ed.buffer.text(), "one\ntwo\nthree\nour\n");
    feed(&mut ed, "<C-w>h");
    assert_eq!(ed.buffer.text(), "one\ntwo\nthree\nour\n");
    assert_eq!(ed.cursor.row, 2, "the other window did not move");

    // A window below the first one, and only it: the two columns are
    // separate trees, so dividing one leaves the other alone.
    assert_eq!(ed.current_window, 0);
    ed.execute_cmdline("sp");
    assert_eq!(ed.windows.len(), 3);
    let rects = ed.window_rects();
    let full = 12 - 1; // the bottom row of the screen is the message row
    assert!(rects[0].height < full, "the left column is divided");
    assert_eq!(rects[1].height, full, "the right column is not");
    assert_eq!(rects[2].x, rects[0].x, "the new window is under the first");
    assert_eq!(rects[2].width, rects[0].width);
    feed(&mut ed, "<C-w>j");
    assert_eq!(ed.current_window, 2);
}

#[test]
fn both_windows_show_the_text_they_are_looking_at() {
    // Two windows on one document, and a file split beside it: the screen
    // has to carry the text of each, not the placeholder of the live one.
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("buffers-pane-one.txt");
    let two = dir.join("buffers-pane-two.txt");
    std::fs::write(&one, "alpha\nbravo\n").unwrap();
    std::fs::write(&two, "one\ntwo\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&one).unwrap();
    ed.viewport.width = 40;
    ed.viewport.height = 8;
    ed.execute_cmdline("sp");
    let composed = ed.compose();
    let row = |y: u16| composed.grid.row_text(y).trim_end().to_string();
    // The window above keeps the row between them for its status line.
    assert_eq!(row(0), "alpha", "the window above has the text");
    assert_eq!(row(1), "bravo");
    assert!(
        row(3).starts_with("-- NORMAL --  buffers-pane-one.txt"),
        "{}",
        row(3)
    );
    assert_eq!(row(4), "alpha", "and so does the one below");
    assert!(
        row(6).starts_with("-- NORMAL --  buffers-pane-one.txt"),
        "{}",
        row(6)
    );

    // A file split beside the current one leaves the current window's
    // document where it was.
    ed.execute_cmdline(&format!("vs {}", two.display()));
    let composed = ed.compose();
    let row = |y: u16| composed.grid.row_text(y).trim_end().to_string();
    assert_eq!(row(0), "alpha", "the window above is untouched");
    // The split went to the window that was up, which was the lower one.
    assert!(row(4).starts_with("alpha"), "{}", row(4));
    assert!(row(4).contains("│one"), "{}", row(4));
    // The second line of each: the left window still has its own document
    // and the new one has the file that was named.
    assert!(
        row(5).trim_start_matches(['│', ' ']).contains("│two"),
        "{}",
        row(5)
    );
}

#[test]
fn closing_a_document_closes_its_windows_and_not_the_others() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("buffers-close-one.txt");
    let two = dir.join("buffers-close-two.txt");
    std::fs::write(&one, "one\n").unwrap();
    std::fs::write(&two, "two\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&one).unwrap();
    ed.execute_cmdline(&format!("e! {}", two.display()));
    ed.execute_cmdline("sp"); // two windows on two.txt
    ed.execute_cmdline("bp"); // the left one goes back to one.txt
    assert_eq!(ed.documents.len(), 2);
    assert_eq!(ed.windows.len(), 2);
    assert_eq!(ed.buffer.text(), "one\n");

    ed.execute_cmdline("bd!"); // closes one.txt and its window
    assert_eq!(ed.documents.len(), 1);
    assert_eq!(ed.windows.len(), 1, "the other window stays");
    assert_eq!(ed.buffer.text(), "two\n");
    assert_eq!(ed.current_window, 0);

    // The last document and window going at once leaves an empty one.
    ed.execute_cmdline("bd!");
    assert_eq!(ed.documents.len(), 1);
    assert_eq!(ed.windows.len(), 1);
    assert_eq!(ed.buffer.text(), "\n");
    assert_eq!(ed.buffer.path(), None);
}

#[test]
fn a_modified_document_is_not_left_behind_by_accident() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("buffers-dirty-one.txt");
    let two = dir.join("buffers-dirty-two.txt");
    std::fs::write(&one, "one\n").unwrap();
    std::fs::write(&two, "other\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&one).unwrap();
    ed.execute_cmdline(&format!("e {}", two.display())); // two documents
    ed.execute_cmdline("bp");
    feed(&mut ed, "ix<Esc>");

    // `:e` and `:bd` both refuse; `!` does what it is told.
    ed.execute_cmdline(&format!("e {}", one.display()));
    assert_eq!(
        ed.message,
        "E37: No write since last change (add ! to override)"
    );
    ed.execute_cmdline("bd");
    assert_eq!(
        ed.message,
        "E37: No write since last change (add ! to override)"
    );
    assert_eq!(ed.documents.len(), 2);

    ed.execute_cmdline("bd!");
    assert_eq!(ed.documents.len(), 1, "the document was closed");
    assert_eq!(
        ed.buffer.text(),
        "other\n",
        "and the other one is on screen"
    );
    assert_eq!(ed.message, "\"buffers-dirty-one.txt\" deleted");

    // `:bn` with only one document open says so rather than doing nothing.
    ed.execute_cmdline("bn");
    assert_eq!(ed.message, "E85: There is no other buffer");
}

#[test]
fn closing_the_last_document_leaves_an_empty_one() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("buffers-last.txt");
    std::fs::write(&path, "text\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&path).unwrap();
    ed.execute_cmdline("bd");
    assert_eq!(ed.documents.len(), 1);
    assert_eq!(
        ed.buffer.text(),
        "\n",
        "one empty line, not a closed editor"
    );
    assert_eq!(ed.buffer.path(), None);
    assert_eq!(ed.cursor.row, 0);
    // And it can be written somewhere new, which is what the window is for.
    // (`:w!`, since the file is there and this buffer never read it.)
    feed(&mut ed, "inew<Esc>");
    ed.execute_cmdline(&format!("w! {}", path.display()));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
}

#[test]
fn reopening_the_same_file_reloads_it_rather_than_duplicating_it() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("buffers-same.txt");
    std::fs::write(&path, "one\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&path).unwrap();
    std::fs::write(&path, "changed on disk\n").unwrap();
    // The same file by the same name, and by a path that leads to the same
    // place: both refill the document instead of opening another one.
    ed.execute_cmdline(&format!("e {}", path.display()));
    assert_eq!(ed.documents.len(), 1);
    assert_eq!(ed.buffer.text(), "changed on disk\n");
    let via_dot = dir.join(".").join("buffers-same.txt");
    ed.execute_cmdline(&format!("e {}", via_dot.display()));
    assert_eq!(ed.documents.len(), 1, "the same file, one document");
}

#[test]
fn a_name_that_is_not_there_is_a_new_file() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let mut ed = Editor::for_tests();
    // A path that is not there yet is a new file, and as the first file it
    // takes the empty window's place rather than being added beside it.
    let fresh = dir.join("brand-new.txt");
    ed.execute_cmdline(&format!("e {}", fresh.display()));
    assert_eq!(ed.documents.len(), 1);
    assert_eq!(
        ed.buffer.path().and_then(|p| p.file_name()),
        Some(std::ffi::OsStr::new("brand-new.txt"))
    );
    assert_eq!(ed.buffer.text(), "\n", "a new file starts empty");

    // A second one, though, is a document of its own.
    let other = dir.join("brand-new-two.txt");
    std::fs::write(&other, "two\n").unwrap();
    ed.execute_cmdline(&format!("e {}", other.display()));
    assert_eq!(ed.documents.len(), 2);
}

#[test]
fn the_saved_state_is_an_identity_not_a_comparison() {
    // Removing a character and typing it back leaves the same text, and it
    // is still an edit: the state it is in is not the state that was
    // written. Comparing the text instead would call this clean and let
    // `:q` walk away from the file the writer meant to keep.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    assert!(!ed.is_dirty());
    feed(&mut ed, "x"); // one -> ne
    feed(&mut ed, "io<Esc>"); // and back to one
    assert_eq!(ed.buffer.text(), "one\n");
    assert!(ed.is_dirty(), "the same text is not the same state");
    feed(&mut ed, "uu");
    assert!(!ed.is_dirty(), "but undoing the steps lands on it");
}

#[test]
fn a_normal_mode_cursor_on_the_line_end_snaps_back_to_the_last_character() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("ab\ncd\n");
    // Even if something parks the cursor on the terminator...
    ed.cursor = Cursor::new(0, 2);
    feed(&mut ed, "x");
    // ...the next normal-mode key first brings it back onto `b`.
    assert_eq!(ed.buffer.text(), "a\ncd\n");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
}

#[test]
fn append_positions_the_cursor_after_the_current_character() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("ab\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "aX<Esc>");
    assert_eq!(ed.buffer.text(), "aXb\n");
    assert_eq!(ed.cursor, Cursor::new(0, 1));
}

#[test]
fn command_line_collects_edits_and_executes_on_enter() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, ":");
    assert_eq!(ed.mode, Mode::Command);
    feed(&mut ed, "wq");
    assert_eq!(ed.cmdline, "wq");
    feed(&mut ed, "<BS>");
    assert_eq!(ed.cmdline, "w");
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.mode, Mode::Normal);
    assert_eq!(ed.cmdline, "");
}

#[test]
fn writing_without_a_file_name_reports_e32() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, ":w<CR>");
    assert_eq!(ed.message, "E32: No file name");
    assert_eq!(ed.mode, Mode::Normal);
    assert!(!ed.should_quit);
}

#[test]
fn quitting_a_dirty_buffer_reports_e37_and_stays() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, "ix<Esc>");
    assert!(ed.is_dirty());
    feed(&mut ed, ":q<CR>");
    assert!(!ed.should_quit);
    assert!(ed.message.starts_with("E37"), "{}", ed.message);
    feed(&mut ed, ":q!<CR>");
    assert!(ed.should_quit);
}

#[test]
fn unknown_command_line_reports_e492() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, ":frobnicate<CR>");
    assert_eq!(ed.message, "E492: Not a command: frobnicate");
}

#[test]
fn a_clean_buffer_quits_immediately() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, ":q<CR>");
    assert_eq!(ed.mode, Mode::Normal, "we are back in normal mode");
    assert!(ed.cmdline.is_empty());
    assert!(ed.should_quit);
}

#[test]
fn unmapped_keys_report_and_clear_pending() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, "<Space>");
    assert_eq!(ed.message, "unmapped key: <Space>");
    // `g` starts a multi-key binding and waits for the rest of it.
    feed(&mut ed, "g");
    assert_eq!(ed.pending, "g");
    // A key name the notation does not know is an error rather than a key,
    // so this uses one it does that is not bound to anything.
    feed(&mut ed, "<Space>");
    assert_eq!(ed.pending, "", "the half-typed sequence is dropped");
    assert_eq!(ed.message, "unmapped key: <Space>");
    // `gq` is one of those bindings, so the same two keys now mean
    // something: the rewrap operator, waiting for a motion.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "gq");
    assert_eq!(ed.pending, "");
    assert_eq!(ed.operator, Some(Operator::Format), "waiting for a motion");
    assert_eq!(ed.message, "");
}

#[test]
fn amp_repeats_the_last_substitution_on_this_line() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one one\ntwo one\n");
    ed.execute_cmdline("s/one/1/g");
    assert_eq!(ed.buffer.text(), "1 1\ntwo one\n");
    // `:&` is the same pattern and replacement on the cursor's line, without
    // the `g` that was used last time.
    feed(&mut ed, "j");
    ed.execute_cmdline("&");
    assert_eq!(ed.buffer.text(), "1 1\ntwo 1\n");

    // With nothing substituted yet there is nothing to repeat.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    ed.execute_cmdline("&");
    assert_eq!(ed.buffer.text(), "one\n");
    assert!(!ed.message.is_empty(), "and it says so: {}", ed.message);
}

#[test]
fn a_substitution_can_use_a_group() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("fn one() {}\n");
    feed(&mut ed, ":s/fn ([a-z]+)/fn changed_\\1<Enter>");
    assert_eq!(ed.buffer.text(), "fn changed_one() {}\n");

    feed(&mut ed, ":%s/(changed)_(one)/\\2_\\1/g<Enter>");
    assert_eq!(ed.buffer.text(), "fn one_changed() {}\n");
}

#[test]
fn a_message_survives_until_the_next_key() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, ":q!<CR>");
    assert!(ed.should_quit);
    ed.should_quit = false;
    assert_eq!(ed.message, "", "the quit command does not set a message");
    ed.message = "hello".into();
    feed(&mut ed, "l");
    assert_eq!(ed.message, "");
}

#[test]
fn composing_after_editing_shows_the_text() {
    let mut ed = Editor::for_tests();
    ed.handle_resize(40, 4);
    feed(&mut ed, "ihi<Esc>");
    let out = ed.compose();
    assert_eq!(out.grid.row_text(0).trim_end(), "hi");
    let status = out.grid.row_text(2);
    assert!(status.starts_with("-- NORMAL --"), "{status}");
    assert!(status.contains("[No Name][+]"), "{status}");
}

#[test]
fn cursor_is_visible_inside_the_composed_grid() {
    let mut ed = Editor::for_tests();
    ed.handle_resize(40, 4);
    feed(&mut ed, "ihello<Esc>");
    let out = ed.compose();
    assert!(out.cursor_visible);
    assert_eq!(out.cursor, (4, 0));
}

#[test]
fn normal_mode_never_leaves_the_cursor_past_the_last_character() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abc\n");
    feed(&mut ed, "$");
    assert_eq!(ed.cursor, Cursor::new(0, 2));
    feed(&mut ed, "l");
    assert_eq!(
        ed.cursor,
        Cursor::new(0, 2),
        "`l` cannot step onto the newline"
    );
    feed(&mut ed, "x");
    assert_eq!(
        ed.buffer.text(),
        "ab\n",
        "`x` deletes the character under the cursor"
    );
}

#[test]
fn an_empty_line_pins_the_normal_cursor_to_column_zero() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("ab\n\ncd\n");
    ed.cursor = Cursor::new(1, 0);
    feed(&mut ed, "x");
    assert_eq!(
        ed.buffer.text(),
        "ab\ncd\n",
        "`x` on an empty line joins it downward"
    );
    assert_eq!(
        ed.cursor,
        Cursor::new(1, 0),
        "and the cursor stays on that line"
    );
}

#[test]
fn a_failed_save_quit_does_not_quit() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("x\n");
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let impossible = dir.join("no-such-dir").join("deep.txt");

    ed.execute_cmdline(&format!("wq {}", impossible.display()));
    assert!(!ed.should_quit, "the session must survive a failed write");
    assert!(ed.message.contains("i/o error"), "{}", ed.message);
}

#[test]
fn deleting_a_crlf_line_takes_the_carriage_return_with_it() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "dd");
    assert_eq!(ed.buffer.text(), "bar\r\n");
}

#[test]
fn opening_a_line_below_a_crlf_line_keeps_the_line_endings_intact() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "o<Esc>");
    assert_eq!(ed.buffer.text(), "foo\r\n\r\nbar\r\n");
}

#[test]
fn a_zero_sized_terminal_does_not_corrupt_the_viewport() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&"line\n".repeat(10));
    ed.cursor = Cursor::new(5, 0);
    ed.handle_resize(0, 0);
    assert_eq!(ed.viewport.top_line, 5, "no meaningless scroll offset");
    assert_eq!(ed.viewport.left_col, 0);
    assert!(!ed.compose().cursor_visible, "nothing is visible");

    ed.handle_resize(80, 10);
    let out = ed.compose();
    assert!(out.cursor_visible);
    assert_eq!(out.cursor, (0, 3), "the cursor is back on screen");
}

#[test]
fn a_zero_width_terminal_still_tracks_the_cursor_column() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&"wide line\n".repeat(10));
    ed.cursor = Cursor::new(5, 2);
    ed.handle_resize(0, 10);
    assert_eq!(
        ed.viewport.top_line, 0,
        "the cursor line is already on screen"
    );
    assert_eq!(ed.viewport.left_col, 2, "no room to scroll into, so pin it");
}

#[test]
fn the_scroll_brings_the_cursor_into_view_by_cells_not_characters() {
    // A line of tabs is several times wider than its characters are long, so a
    // cursor a few characters in is well past the right edge of a ten-cell
    // window. Counting characters there scrolls too little and leaves the
    // cursor off the screen — the wart the roadmap called "a column off".
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("\t\tabcdefghij\n");
    ed.handle_resize(10, 3);
    ed.cursor = Cursor::new(0, 8);
    ed.after_edit();
    assert_eq!(
        ed.viewport.left_col, 5,
        "the `g` is the fifteenth cell, and ten of them fit"
    );
    assert!(ed.compose().cursor_visible);
}

#[test]
fn edit_refuses_to_discard_unsaved_changes() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("reload.txt");
    std::fs::write(&path, "on disk\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&path).unwrap();
    feed(&mut ed, "ix<Esc>");
    assert!(ed.is_dirty());

    ed.execute_cmdline(&format!("e {}", path.display()));
    assert!(ed.message.starts_with("E37"), "{}", ed.message);
    assert_eq!(ed.buffer.text(), "xon disk\n", "the edits are still here");

    ed.execute_cmdline(&format!("e! {}", path.display()));
    assert_eq!(ed.buffer.text(), "on disk\n", "`e!` reloads from disk");
    assert!(!ed.is_dirty());
}

#[test]
fn edit_on_a_clean_buffer_reloads() {
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clean.txt");
    std::fs::write(&path, "one\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.load_file(&path).unwrap();
    std::fs::write(&path, "two\n").unwrap();
    ed.execute_cmdline(&format!("e {}", path.display()));
    assert_eq!(ed.buffer.text(), "two\n");
}

/// Deterministic pseudo-random key sequence: nothing may panic, and the
/// buffer/viewport invariants must hold after every single key.

#[test]
fn random_key_sequences_keep_the_editor_consistent() {
    use inkstone_rpc::{KeyCode, KeyMsg};

    let alphabet: Vec<KeyMsg> = "iIaAoOxddwbgG:u"
        .chars()
        .map(|c| KeyMsg::new(KeyCode::Char(c)))
        .chain([
            KeyMsg::new(KeyCode::Esc),
            KeyMsg::new(KeyCode::Enter),
            KeyMsg::new(KeyCode::Backspace),
            KeyMsg::new(KeyCode::Tab),
            KeyMsg::new(KeyCode::Left),
            KeyMsg::new(KeyCode::Right),
            KeyMsg::new(KeyCode::Up),
            KeyMsg::new(KeyCode::Down),
            KeyMsg::new(KeyCode::Home),
            KeyMsg::new(KeyCode::End),
            KeyMsg::new(KeyCode::Delete),
            KeyMsg::ctrl('w'),
            KeyMsg::ctrl('r'),
        ])
        .collect();

    let mut ed = Editor::for_tests();
    ed.handle_resize(24, 6);
    let mut seed = 0x2545_f491_4f6c_dd1du64;

    for step in 0..4000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let r = seed as usize;

        if r % 19 == 0 {
            ed.handle_resize((r % 40) as u16, (r % 14) as u16);
        }
        let key = alphabet[r % alphabet.len()].clone();
        ed.handle_key(key);
        ed.should_quit = false; // `:q!` must not end the run

        let text = ed.buffer.text();
        assert!(
            text.ends_with('\n'),
            "step {step}: buffer lost its final newline"
        );
        assert!(ed.buffer.line_count() >= 1, "step {step}");
        assert!(
            ed.cursor.row < ed.buffer.line_count(),
            "step {step}: row {} of {}",
            ed.cursor.row,
            ed.buffer.line_count()
        );
        assert!(
            ed.cursor.col <= ed.buffer.line_len(ed.cursor.row),
            "step {step}: col {} past line length {}",
            ed.cursor.col,
            ed.buffer.line_len(ed.cursor.row)
        );
        if ed.mode == Mode::Normal {
            let len = ed.buffer.line_len(ed.cursor.row);
            assert!(
                ed.cursor.col < len.max(1),
                "step {step}: normal mode cursor sits past the last character"
            );
        }
        let composed = ed.compose();
        assert_eq!(
            composed.grid.cells.len(),
            composed.grid.width as usize * composed.grid.height as usize
        );
        if composed.cursor_visible {
            assert!(composed.cursor.0 < composed.grid.width.max(1));
            assert!(composed.cursor.1 < composed.grid.height.max(1));
        }
    }
}
