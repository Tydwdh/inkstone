use super::*;

#[test]
fn a_range_can_be_moved_a_level_in_or_out() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    // `>>` moves one line in; a count moves that many.
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "<gt><gt>");
    assert_eq!(ed.buffer.text(), "\tone\ntwo\nthree\n");
    // A count is lines, as in vim: `2>>` moves two of them in.
    feed(&mut ed, "2<gt><gt>");
    assert_eq!(ed.buffer.text(), "\t\tone\n\ttwo\nthree\n");
    // `<<` takes a level off, and one edit takes it all back.
    ed.buffer = Buffer::from_text("\tone\n\t\ttwo\n\tthree\n");
    ed.cursor = Cursor::new(2, 0);
    feed(&mut ed, "<lt><lt>");
    assert_eq!(ed.buffer.text(), "\tone\n\t\ttwo\nthree\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "\tone\n\t\ttwo\n\tthree\n", "one edit");
    // A line indented with spaces loses a tab stop's worth, or all of them.
    ed.buffer = Buffer::from_text("  two spaces\n    four\n\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "<lt>j");
    assert_eq!(
        ed.buffer.text(),
        "two spaces\nfour\n\n",
        "two spaces is less than a stop, so all of them go"
    );
    // A blank line stays blank: indenting nothing is not indentation.
    ed.buffer = Buffer::from_text("\n");
    feed(&mut ed, "<gt><gt>");
    assert_eq!(ed.buffer.text(), "\n");
    // A motion, over the lines it covers.
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "<gt>j");
    assert_eq!(ed.buffer.text(), "\tone\n\ttwo\nthree\n");
}

#[test]
fn set_all_shows_every_setting_and_set_inv_turns_one_around() {
    let mut ed = Editor::for_tests();
    let windows = ed.windows.len();
    let documents = ed.documents.len();
    ed.execute_cmdline("set all");
    assert_eq!(ed.windows.len(), windows + 1, "in a window of its own");
    assert_eq!(ed.documents.len(), documents + 1);
    assert_eq!(ed.list, Some(List::Settings), "and it is a list");
    let text = ed.buffer.text();
    for setting in crate::settings::Setting::ALL.iter().copied() {
        assert!(
            text.contains(setting.name()),
            "{} is in the list: {text}",
            setting.name()
        );
    }
    assert!(text.contains("tabstop"), "{text}");
    assert!(text.contains("scrolloff"), "{text}");
    assert!(ed.message.contains("settings"), "{}", ed.message);
    // A list is not for editing, which is the same rule every list follows.
    let before = ed.buffer.text();
    ed.type_text("x");
    assert!(ed.message.starts_with("E21"), "{}", ed.message);
    assert_eq!(ed.buffer.text(), before, "and nothing changed");
    // `:set inv{name}` turns a switch the other way, and says what it is now.
    ed.execute_cmdline("cclose");
    assert!(!ed.options.borrow().number);
    ed.execute_cmdline("set invnumber");
    assert!(ed.options.borrow().number);
    assert_eq!(ed.message, "number=true");
    ed.execute_cmdline("set invnumber");
    assert!(!ed.options.borrow().number);
    assert_eq!(ed.message, "number=false");
    // A setting that is not a switch has no other way to be.
    ed.execute_cmdline("set invtextwidth");
    assert!(
        ed.message.starts_with("E474") && ed.message.contains("switch"),
        "{}",
        ed.message
    );
    assert_eq!(ed.options.borrow().textwidth.0, 79, "and it is left alone");
    ed.execute_cmdline("set invnope");
    assert!(ed.message.contains("E518"), "{}", ed.message);
}
