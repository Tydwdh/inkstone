use super::*;

#[test]
fn the_register_remembers_what_a_delete_took() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "dw");
    assert_eq!(ed.buffer.text(), "two\n");
    // `dw` took characters, so `p` puts them back into the line, after the
    // character under the cursor.
    feed(&mut ed, "p");
    assert_eq!(ed.buffer.text(), "tone wo\n");
    feed(&mut ed, "P");
    assert_eq!(
        ed.buffer.text(),
        "toneone  wo\n",
        "`P` puts it back before the cursor, which `p` left on the space"
    );
}

#[test]
fn a_put_with_an_empty_register_changes_nothing() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "p");
    assert_eq!(ed.buffer.text(), "one\n");
    assert_eq!(ed.message, "");
}

#[test]
fn a_register_keeps_what_a_named_yank_took() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "\"ayyj");
    assert_eq!(
        ed.register.text(),
        "one\n",
        "the unnamed register took it too"
    );
    assert_eq!(
        ed.registers.get('a').map(|r| r.text().to_string()),
        Some("one\n".to_string())
    );
    // And `\"ap` puts *that* register, not the nonsense the unnamed one has
    // picked up since.
    feed(&mut ed, "dd");
    assert_eq!(ed.buffer.text(), "one\n", "the line went");
    assert_eq!(ed.register.text(), "two\n", "and the unnamed one has it");
    feed(&mut ed, "\"ap");
    assert_eq!(
        ed.buffer.text(),
        "one\none\n",
        "register `a` puts its own text"
    );
}

#[test]
fn the_black_hole_swallows_a_delete() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "yy"); // the unnamed register holds `one`
    feed(&mut ed, "j\"_dd");
    assert_eq!(ed.buffer.text(), "one\n");
    assert_eq!(ed.register.text(), "one\n", "the unnamed one is untouched");
    assert_eq!(
        ed.registers.get('1').map(|r| r.text().to_string()),
        None,
        "and no history entry"
    );
    feed(&mut ed, "p");
    assert_eq!(ed.buffer.text(), "one\none\n", "so `p` still puts the yank");
}

#[test]
fn the_numbered_and_special_registers_remember_for_themselves() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    feed(&mut ed, "yyjdd"); // yank `one`, delete `two`
    assert_eq!(
        ed.registers.get('0').map(|r| r.text().to_string()),
        Some("one\n".to_string())
    );
    assert_eq!(
        ed.registers.get('1').map(|r| r.text().to_string()),
        Some("two\n".to_string())
    );
    assert_eq!(ed.register.text(), "two\n", "the unnamed one is the delete");

    // `\"0p` puts the last *yank* back, whatever was deleted after it.
    feed(&mut ed, "\"0p");
    assert_eq!(ed.buffer.text(), "one\nthree\none\n");

    // `\"+y` hands the text to the frontend, which is the half of the
    // clipboard the kernel cannot reach by itself.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("copied\n");
    assert_eq!(ed.clipboard, None);
    feed(&mut ed, "\"+yy");
    assert_eq!(ed.clipboard.as_deref(), Some("copied\n"));
    assert_eq!(
        ed.registers.clipboard().text(),
        "copied\n",
        "and it is kept"
    );

    // A paste from the terminal is what the clipboard holds, so `\"+p` can
    // put back what the terminal just pasted.
    ed.note_clipboard_text("from the system\n");
    assert_eq!(ed.registers.clipboard().text(), "from the system\n");
    feed(&mut ed, "\"+p");
    assert!(
        ed.buffer.text().contains("from the system\n"),
        "{}",
        ed.buffer.text()
    );
}

#[test]
fn a_register_name_that_is_not_one_is_reported() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "\"!");
    assert_eq!(ed.message, "E354: Invalid register name: !");
    assert_eq!(ed.register_prefix, crate::register::Prefix::None);
    feed(&mut ed, "x");
    assert_eq!(ed.buffer.text(), "ne\n", "and the next key works normally");
}

#[test]
fn reg_lists_what_the_registers_hold() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "\"ayy");
    ed.execute_cmdline("reg");
    assert!(
        ed.message.starts_with("--- Registers ---"),
        "{}",
        ed.message
    );
    assert!(
        ed.message.contains("\"a   one^J linewise"),
        "{}",
        ed.message
    );
    assert!(ed.message.contains("\"0   one^J"), "{}", ed.message);
}
