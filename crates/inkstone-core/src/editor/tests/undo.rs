use super::*;

#[test]
fn a_list_of_edits_is_applied_backwards_as_one_step() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    // Two edits, given in document order, and both relative to the text as
    // it was: the later one must not move the earlier one's offsets.
    let edits = vec![(0, 3, "1".to_string()), (8, 13, "3".to_string())];
    assert_eq!(ed.apply_edits(&edits), 2);
    assert_eq!(ed.buffer.text(), "1\ntwo\n3\n");
    // One step to undo, however many edits there were.
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\ntwo\nthree\n", "all of it");
    // An edit that inserts just before the last line break — 13 is where
    // that break is — and one that deletes everything.
    let edits = vec![(13, 13, "!".to_string())];
    assert_eq!(ed.apply_edits(&edits), 1);
    assert_eq!(ed.buffer.text(), "one\ntwo\nthree!\n");
    // Deleting everything leaves the one empty line a buffer always has.
    let len = ed.buffer.len_chars();
    let edits = vec![(0, len, String::new())];
    assert_eq!(ed.apply_edits(&edits), 1);
    assert_eq!(
        ed.buffer.text(),
        "\n",
        "an empty document is one empty line"
    );
    // Nothing asked for is nothing done.
    assert_eq!(ed.apply_edits(&[]), 0);
}

#[test]
fn pasted_text_is_one_step() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.cursor = Cursor::new(0, 0);
    // What a paste, or an input method's commit, hands the daemon.
    ed.type_text("a\npasted\nblock");
    assert_eq!(ed.buffer.text(), "a\npasted\nblockone\ntwo\n");
    assert_eq!(ed.cursor, Cursor::new(2, 5), "and the cursor follows it");

    ed.undo();
    assert_eq!(ed.buffer.text(), "one\ntwo\n", "one `u` takes it all back");
}

#[test]
fn a_put_is_one_step_to_undo() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "yyp");
    assert_eq!(ed.buffer.text(), "one\none\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\n", "one `u` takes the whole paste");
    assert_eq!(
        ed.register.text(),
        "one\n",
        "and the register still holds it"
    );
}

#[test]
fn undo_and_redo_put_the_text_and_the_cursor_back() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abc\n");
    ed.cursor = Cursor::new(0, 1);
    feed(&mut ed, "x");
    assert_eq!(ed.buffer.text(), "ac\n");

    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "abc\n");
    assert_eq!(ed.cursor, Cursor::new(0, 1), "the cursor comes back too");

    feed(&mut ed, "<C-r>");
    assert_eq!(ed.buffer.text(), "ac\n");
    assert_eq!(ed.cursor, Cursor::new(0, 1));
}

#[test]
fn an_insert_session_is_one_step() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, "ihello<Esc>");
    assert_eq!(ed.buffer.text(), "hello\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "\n", "typing is one step, not five");
}

#[test]
fn typing_and_backspacing_over_it_is_still_one_step() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, "iabc<BS><Esc>");
    assert_eq!(ed.buffer.text(), "ab\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "\n", "the whole session comes back out");
}

#[test]
fn a_line_opened_and_typed_into_is_one_step() {
    // `o` writes the line break and then the typing lands in front of it:
    // two edits, one thing the person did, so one `u`.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "ohello<Esc>");
    assert_eq!(ed.buffer.text(), "one\nhello\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\n", "the whole line came back out");
    feed(&mut ed, "<C-r>");
    assert_eq!(ed.buffer.text(), "one\nhello\n", "and redo puts it back");

    // The same for a line above, and for typing then backspacing over part
    // of it in a session that was opened that way.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "Oabove<Esc>");
    assert_eq!(ed.buffer.text(), "above\none\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\n");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "ohello<BS><BS><Esc>");
    assert_eq!(ed.buffer.text(), "one\nhel\n");
    feed(&mut ed, "u");
    assert_eq!(
        ed.buffer.text(),
        "one\n",
        "still one step after backspacing"
    );
}

#[test]
fn changing_a_word_and_typing_the_new_one_is_one_step() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two three\n");
    feed(&mut ed, "wcwX<Esc>");
    assert_eq!(ed.buffer.text(), "one X three\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one two three\n", "the word is back");
    feed(&mut ed, "<C-r>");
    assert_eq!(ed.buffer.text(), "one X three\n");
}

#[test]
fn dot_repeats_a_change_that_took_a_character_find() {
    // The find is part of the motion the operator took, and the character it
    // looked for is remembered, so `.` finds its own comma again.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one, two, three, four\n");
    feed(&mut ed, "df,");
    assert_eq!(ed.buffer.text(), " two, three, four\n");
    feed(&mut ed, ".");
    assert_eq!(ed.buffer.text(), " three, four\n", "the next comma, again");
}

#[test]
fn dot_repeats_the_last_change() {
    // A delete, repeated with a count.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abcdefgh\n");
    feed(&mut ed, "x");
    assert_eq!(ed.buffer.text(), "bcdefgh\n");
    feed(&mut ed, ".");
    assert_eq!(ed.buffer.text(), "cdefgh\n", "the same change again");
    feed(&mut ed, "3.");
    assert_eq!(ed.buffer.text(), "fgh\n", "and with a count");

    // A line delete, from wherever the cursor is now.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    feed(&mut ed, "dd");
    assert_eq!(ed.buffer.text(), "two\nthree\n");
    feed(&mut ed, "j.");
    assert_eq!(ed.buffer.text(), "two\n", "the line under the cursor");

    // A motion in between does not count as a change.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("aa bb cc\n");
    feed(&mut ed, "x");
    assert_eq!(ed.buffer.text(), "a bb cc\n");
    feed(&mut ed, "w.");
    assert_eq!(
        ed.buffer.text(),
        "a b cc\n",
        "the change, where the cursor went"
    );

    // With nothing changed yet, `.` does nothing at all.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, ".");
    assert_eq!(ed.buffer.text(), "one\n");
}

#[test]
fn dot_repeats_the_text_an_insert_session_typed() {
    // `cw` and the text after it are one change.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two three\n");
    feed(&mut ed, "wcwXXX<Esc>");
    assert_eq!(ed.buffer.text(), "one XXX three\n");
    feed(&mut ed, "w.");
    assert_eq!(ed.buffer.text(), "one XXX XXX\n", "the word and the text");

    // `i`, `a` and `o` carry their text along the same way.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "i[[<Esc>.");
    assert_eq!(ed.buffer.text(), "[[[[one\ntwo\n");
    feed(&mut ed, "jA]]<Esc>.");
    assert_eq!(ed.buffer.text(), "[[[[one\ntwo]]]]\n");
    feed(&mut ed, "joline<Esc>.");
    assert_eq!(ed.buffer.text(), "[[[[one\ntwo]]]]\nline\nline\n");

    // A backspace in the session is part of it, and `.` types the session's
    // keys again — so the text ends up the same way it did the first time.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "iab<BS><BS>done<Esc>");
    assert_eq!(ed.buffer.text(), "doneone\ntwo\n");
    feed(&mut ed, "j0.");
    assert_eq!(ed.buffer.text(), "doneone\ndonetwo\n");

    // A change that is not a session forgets the last session's text.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "iXX<Esc>");
    feed(&mut ed, "x");
    feed(&mut ed, ".");
    // The delete ran again rather than the text of the session: `.` repeats
    // the *change* that came last, and `x` came after the typing.
    assert_eq!(ed.buffer.text(), "Xne\n");
}

#[test]
fn dot_keeps_the_change_through_a_yank_and_an_undo() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abcdef\n");
    feed(&mut ed, "x");
    // A yank changes nothing and is not what `.` repeats.
    feed(&mut ed, "yiw");
    feed(&mut ed, ".");
    assert_eq!(ed.buffer.text(), "cdef\n");
    // Undoing is not a change either: the change that is remembered is the
    // one that was made, so `.` puts it back.
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "bcdef\n");
    feed(&mut ed, ".");
    assert_eq!(ed.buffer.text(), "cdef\n", "`.` re-applies the change");
}

#[test]
fn undoing_a_line_deletion_puts_its_terminator_back() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
    feed(&mut ed, "jdd");
    assert_eq!(ed.buffer.text(), "foo\r\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "foo\r\nbar\r\n");

    // `dd` on the only line takes the buffer's last newline with it, and
    // the newline the invariant hands back belongs to the step: undo must
    // not leave a second one behind.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abc\n");
    feed(&mut ed, "dd");
    assert_eq!(ed.buffer.text(), "\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "abc\n", "not \"abc\\n\\n\"");
}

#[test]
fn undo_and_redo_report_when_they_run_out() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, "u");
    assert_eq!(ed.message, "Already at oldest change");

    feed(&mut ed, "ix<Esc>");
    feed(&mut ed, "<C-r>");
    assert_eq!(ed.message, "Already at newest change");
}

#[test]
fn an_edit_after_an_undo_drops_the_redo_stack() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abc\n");
    feed(&mut ed, "x");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "abc\n");
    feed(&mut ed, "x");
    feed(&mut ed, "<C-r>");
    assert_eq!(ed.buffer.text(), "bc\n", "redo had nothing to replay");
    assert_eq!(ed.message, "Already at newest change");
}

#[test]
fn undo_walks_all_the_way_back_and_redo_walks_forward_again() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\nthree\n");
    let original = ed.buffer.text();

    feed(&mut ed, "iXY<Esc>wddGA!<Esc>odelete me<Esc>xdd");
    let edited = ed.buffer.text();
    assert_ne!(original, edited, "the sequence has to change something");

    while ed.undo() {}
    assert_eq!(ed.buffer.text(), original, "step by step back to the start");
    while ed.redo() {}
    assert_eq!(ed.buffer.text(), edited, "and forward to the end");
}

#[test]
fn an_edit_after_an_undo_keeps_the_branch() {
    // The point of the tree: what was undone is not thrown away by the next
    // edit. `u` walks the branch the text is on; `g-` walks the order the states
    // were made in, which is how a person gets back to it.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "x");
    assert_eq!(ed.buffer.text(), "ne\ntwo\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\ntwo\n");

    // A different edit: this is a *new* branch, and redo has nothing after it.
    feed(&mut ed, "dd");
    assert_eq!(ed.buffer.text(), "two\n");
    assert!(!ed.redo(), "nothing was made after this state");

    // `g-` goes one state back in the order they were made, which is the undone
    // one — not the state this branch came from.
    feed(&mut ed, "g-");
    assert_eq!(ed.buffer.text(), "ne\ntwo\n", "the branch is still there");
    feed(&mut ed, "g+");
    assert_eq!(ed.buffer.text(), "two\n", "and forward again");
    // `u` is the branch, not time: it takes the `dd` back.
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\ntwo\n");

    // The count repeats, and the ends say so rather than guessing.
    feed(&mut ed, "g-");
    assert_eq!(ed.buffer.text(), "one\ntwo\n", "the state before the `x`");
    feed(&mut ed, "3g-");
    assert_eq!(ed.message, "E831: Cannot go back that far");
    feed(&mut ed, ":later 5<Enter>");
    assert_eq!(ed.message, "E831: Cannot go forward that far");
}

#[test]
fn undolist_names_the_leaves_of_the_tree() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "x");
    feed(&mut ed, "u");
    feed(&mut ed, "dd");
    feed(&mut ed, ":undolist<Enter>");
    assert!(ed.message.starts_with("2 leaves"), "{}", ed.message);
    // The two leaves are the `x` branch and the `dd` one — the state the undo
    // went back to is not a leaf, since something was made from it. The numbers
    // are the ones `:undo` takes: state 0 is the buffer as it arrived.
    assert!(ed.message.contains("#1: 1 change "), "{}", ed.message);
    assert!(ed.message.contains("#2: 1 change "), "{}", ed.message);
    assert_eq!(ed.message.matches("change ").count(), 2, "{}", ed.message);
    feed(&mut ed, ":undo 2<Enter>");
    assert_eq!(ed.buffer.text(), "two\n", "the branch, by its number");
    feed(&mut ed, ":undo 1<Enter>");
    assert_eq!(ed.buffer.text(), "ne\ntwo\n", "and the other one");
    feed(&mut ed, ":undo 99<Enter>");
    assert_eq!(ed.message, "E830: Undo number 99 not found");
}

#[test]
fn earlier_and_later_understand_a_time() {
    // `:earlier 10s` and `:later 1h` are vim's time forms: the state about that
    // long before or after the one the text is in. A test cannot wait ten
    // seconds, so what is checked here is the shape of the rule — a count in
    // states, a time that is small enough to be the same state, and a refusal
    // that names what it did not understand.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    feed(&mut ed, "x");
    feed(&mut ed, "u");
    // `:normal` runs its keys from the beginning of the line, a trap this
    // project has met before: `dd` takes the first line, not the one the `x` was
    // on.
    feed(&mut ed, ":normal dd<Enter>");
    assert_eq!(ed.buffer.text(), "two\nthree\n");

    // Older than everything: the state the buffer arrived in.
    feed(&mut ed, ":earlier 10s<Enter>");
    assert_eq!(ed.buffer.text(), "one\ntwo\nthree\n");
    // Newer than everything: the newest state there is.
    feed(&mut ed, ":later 10s<Enter>");
    assert_eq!(ed.buffer.text(), "two\nthree\n");
    // A count in states is what `:earlier 1` means.
    feed(&mut ed, ":earlier 1<Enter>");
    assert_eq!(ed.buffer.text(), "ne\ntwo\nthree\n", "one state back");
    // A unit that is not one says so.
    feed(&mut ed, ":earlier 3q<Enter>");
    assert!(
        ed.message.starts_with("E475: cannot go back by"),
        "{}",
        ed.message
    );
}
