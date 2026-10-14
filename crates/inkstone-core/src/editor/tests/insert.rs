use super::*;

#[test]
fn an_insert_mapping_of_several_keys_fires() {
    let mut ed = Editor::for_tests();
    ed.keymap
        .borrow_mut()
        .bind(Mode::Insert, "jk", vec!["escape".into()])
        .unwrap();
    feed(&mut ed, "ijk");
    assert_eq!(ed.buffer.text(), "\n", "the mapped keys typed nothing");
    assert_eq!(ed.mode, Mode::Normal, "and the mapping ran");
}

#[test]
fn typing_in_insert_mode_edits_the_buffer() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, "ihello<Esc>");
    assert_eq!(ed.buffer.text(), "hello\n");
    assert_eq!(ed.mode, Mode::Normal);
    assert_eq!(ed.cursor, Cursor::new(0, 4), "escape steps back one column");
    assert!(ed.is_dirty());
    assert!(!ed.message.contains("unmapped"), "{}", ed.message);
}

#[test]
fn escape_only_steps_back_when_leaving_insert() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, "ii");
    assert_eq!(ed.buffer.text(), "i\n");
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    // Pressing escape again in normal mode leaves the cursor alone.
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
}

#[test]
fn enter_and_backspace_in_insert_mode() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, "iab<CR>cd<Esc>");
    assert_eq!(ed.buffer.text(), "ab\ncd\n");
    assert_eq!(ed.cursor, Cursor::new(1, 1));

    // Backspace eats the character before the cursor, one grapheme at a time.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abc\n");
    ed.cursor = Cursor::new(0, 2);
    feed(&mut ed, "i<BS><Esc>");
    assert_eq!(ed.buffer.text(), "ac\n");
    assert_eq!(
        ed.cursor,
        Cursor::new(0, 0),
        "insert mode ended, so the cursor stepped back"
    );
    feed(&mut ed, "li<BS><Esc>");
    assert_eq!(ed.buffer.text(), "c\n");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
}

#[test]
fn backspace_at_the_start_of_a_line_joins_lines() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("ab\ncd\n");
    ed.cursor = Cursor::new(1, 0);
    feed(&mut ed, "i<BS><Esc>");
    assert_eq!(ed.buffer.text(), "abcd\n");
    assert_eq!(ed.cursor, Cursor::new(0, 1));
}

#[test]
fn x_takes_the_character_but_an_insert_delete_does_not() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "xp");
    assert_eq!(ed.buffer.text(), "noe\n", "`x` fills the register");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "yy");
    feed(&mut ed, "i<Del><Esc>");
    assert_eq!(ed.buffer.text(), "ne\ntwo\n", "insert mode's delete edits");
    feed(&mut ed, "p");
    assert_eq!(
        ed.buffer.text(),
        "ne\none\ntwo\n",
        "and it left the yanked line where it was"
    );
}

#[test]
fn separate_insert_sessions_are_separate_steps() {
    let mut ed = Editor::for_tests();
    feed(&mut ed, "ione<Esc>");
    feed(&mut ed, "Atwo<Esc>");
    assert_eq!(ed.buffer.text(), "onetwo\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "\n");
}

#[test]
fn open_line_commands_enter_insert_mode() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.cursor = Cursor::new(0, 1);
    feed(&mut ed, "o");
    assert_eq!(ed.mode, Mode::Insert);
    assert_eq!(ed.buffer.text(), "one\n\ntwo\n");
    assert_eq!(ed.cursor, Cursor::new(1, 0));
    // Leaving insert mode steps back onto the line above, as in vim.
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.cursor, Cursor::new(0, 2));
    feed(&mut ed, "O");
    assert_eq!(ed.buffer.text(), "\none\n\ntwo\n");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    assert_eq!(ed.mode, Mode::Insert);
}

#[test]
fn insert_mode_may_sit_one_past_the_last_character() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abc\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "A");
    assert_eq!(ed.cursor, Cursor::new(0, 3));
    feed(&mut ed, "X<Esc>");
    assert_eq!(ed.buffer.text(), "abcX\n");
}

#[test]
fn leaving_insert_mode_steps_back_onto_a_real_character() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abc\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "A<Esc>");
    assert_eq!(ed.cursor, Cursor::new(0, 2));
    assert_eq!(ed.mode, Mode::Normal);
}

#[test]
fn backspace_at_the_start_of_a_crlf_line_takes_the_whole_terminator() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
    ed.cursor = Cursor::new(1, 0);
    feed(&mut ed, "i<BS><Esc>");
    assert_eq!(ed.buffer.text(), "foobar\r\n", "no stray carriage return");
}

#[test]
fn the_delete_key_deletes_forward_in_insert_mode() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abc\n");
    ed.cursor = Cursor::new(0, 1);
    feed(&mut ed, "i<Del><Esc>");
    assert_eq!(ed.buffer.text(), "ac\n");
}

#[test]
fn arrow_keys_move_the_cursor_in_insert_mode() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "i");
    assert_eq!(ed.mode, Mode::Insert);

    feed(&mut ed, "<Right><Down>");
    assert_eq!(ed.cursor, Cursor::new(1, 1));
    assert_eq!(
        ed.mode,
        Mode::Insert,
        "navigating does not leave insert mode"
    );
    assert_eq!(ed.buffer.text(), "one\ntwo\n", "and it does not edit");

    feed(&mut ed, "<Up><Left><Home><End>");
    assert_eq!(ed.cursor, Cursor::new(0, 2));
    assert_eq!(ed.mode, Mode::Insert);

    // Typing after navigating inserts at the new position.
    feed(&mut ed, "X<Esc>");
    assert_eq!(ed.buffer.text(), "onXe\ntwo\n");
}

#[test]
fn append_at_the_end_of_a_crlf_line_inserts_before_the_carriage_return() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "AX<Esc>");
    assert_eq!(ed.buffer.text(), "fooX\r\nbar\r\n");
}
