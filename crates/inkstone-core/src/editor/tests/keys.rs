use super::*;

#[test]
fn a_sequence_that_is_not_a_mapping_is_typed_after_all() {
    let mut ed = Editor::for_tests();
    ed.keymap
        .borrow_mut()
        .bind(Mode::Insert, "jk", vec!["escape".into()])
        .unwrap();
    feed(&mut ed, "ijx");
    assert_eq!(
        ed.buffer.text(),
        "jx\n",
        "the held-back key is typed when the next one proves the sequence"
    );
    assert_eq!(ed.mode, Mode::Insert, "and the mapping did not run");
}

#[test]
fn a_held_back_key_is_replayed_the_way_it_was_pressed() {
    let mut ed = Editor::for_tests();
    ed.keymap
        .borrow_mut()
        .bind(Mode::Insert, "jk", vec!["escape".into()])
        .unwrap();
    // `j` then a backspace: not a mapping, so `j` is typed and the
    // backspace deletes it, in that order.
    feed(&mut ed, "ij<BS>x");
    assert_eq!(ed.buffer.text(), "x\n");
    assert_eq!(ed.mode, Mode::Insert);
}

#[test]
fn a_key_bound_on_its_own_runs_on_its_own() {
    let mut ed = Editor::for_tests();
    {
        let mut keymap = ed.keymap.borrow_mut();
        keymap
            .bind(Mode::Insert, "j", vec!["escape".into()])
            .unwrap();
        keymap
            .bind(Mode::Insert, "jk", vec!["escape".into()])
            .unwrap();
    }
    // The editor has no clock, so nothing waits to see whether a second key
    // is coming: a key that is bound on its own is answered at once, and the
    // longer sequence behind it cannot fire. A user who wants `jk` should
    // not also bind `j`.
    feed(&mut ed, "ij");
    assert_eq!(ed.mode, Mode::Normal, "the one-key mapping ran");
    assert_eq!(ed.buffer.text(), "\n");
}

#[test]
fn a_mapping_clears_the_keys_it_was_holding() {
    let mut ed = Editor::for_tests();
    ed.keymap
        .borrow_mut()
        .bind(Mode::Insert, "jk", vec!["escape".into()])
        .unwrap();
    feed(&mut ed, "ijk");
    assert!(ed.insert_pending.is_empty());
    feed(&mut ed, "ihello<Esc>");
    assert_eq!(ed.buffer.text(), "hello\n", "and typing works again");
}

#[test]
fn the_normal_modes_multi_key_binding_still_works() {
    // The two pending mechanisms are separate: `dd` is a normal-mode
    // sequence, and it is not confused by the insert-mode one.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "dd");
    assert_eq!(ed.buffer.text(), "two\n");
    assert!(ed.insert_pending.is_empty());
}

#[test]
fn cursor_keys_work_too() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("ab\ncd\n");
    feed(&mut ed, "<Down><Right>");
    assert_eq!(ed.cursor, Cursor::new(1, 1));
    feed(&mut ed, "<Home>");
    assert_eq!(ed.cursor, Cursor::new(1, 0));
    feed(&mut ed, "<End>");
    assert_eq!(ed.cursor, Cursor::new(1, 1), "`<End>` is vim's `$`");
}
