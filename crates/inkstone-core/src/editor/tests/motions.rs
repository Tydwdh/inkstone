use super::*;

#[test]
fn movement_keys() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\nthree\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "w");
    assert_eq!(ed.cursor, Cursor::new(0, 4));
    feed(&mut ed, "l");
    assert_eq!(ed.cursor, Cursor::new(0, 5));
    feed(&mut ed, "$");
    assert_eq!(ed.cursor.col, 6, "`$` lands on the last character");
    feed(&mut ed, "0");
    assert_eq!(ed.cursor.col, 0);
    feed(&mut ed, "j");
    assert_eq!(ed.cursor.row, 1);
    feed(&mut ed, "G");
    assert_eq!(ed.cursor.row, 1);
    feed(&mut ed, "gg");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
}

#[test]
fn a_number_can_be_counted_with() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("count = 41;\n");
    // The number after the cursor, not only under it.
    feed(&mut ed, "<C-a>");
    assert_eq!(ed.buffer.text(), "count = 42;\n");
    assert_eq!(ed.cursor.col, 8, "and the cursor is left on the number");
    // A count, a sighted minus, and going back down.
    feed(&mut ed, "5<C-a>");
    assert_eq!(ed.buffer.text(), "count = 47;\n", "47 + 5");
    feed(&mut ed, "10<C-x>");
    assert_eq!(ed.buffer.text(), "count = 37;\n", "47 - 10");
    ed.buffer = Buffer::from_text("let x = -3;\n");
    feed(&mut ed, "<C-x>");
    assert_eq!(
        ed.buffer.text(),
        "let x = -4;\n",
        "the minus is the number's"
    );
    // The cursor in the middle of a number edits all of it.
    ed.buffer = Buffer::from_text("n = 100;\n");
    ed.cursor = Cursor::new(0, 5);
    feed(&mut ed, "<C-a>");
    assert_eq!(ed.buffer.text(), "n = 101;\n");
    // Nothing to count: said so, and nothing changes.
    ed.buffer = Buffer::from_text("no digits here\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "<C-a>");
    assert_eq!(ed.buffer.text(), "no digits here\n");
    assert_eq!(ed.message, "no number here");
}

#[test]
fn the_case_operators_take_a_motion_a_text_object_or_a_line() {
    let mut ed = Editor::for_tests();
    // A motion.
    ed.buffer = Buffer::from_text("one two three\n");
    ed.cursor = Cursor::new(0, 4);
    feed(&mut ed, "gUw");
    assert_eq!(ed.buffer.text(), "one TWO three\n");
    feed(&mut ed, "guw");
    assert_eq!(ed.buffer.text(), "one two three\n", "and back");
    // A count, and a motion that runs to the end of the line.
    feed(&mut ed, "gU$");
    assert_eq!(ed.buffer.text(), "one TWO THREE\n");
    feed(&mut ed, "0wg~$");
    assert_eq!(ed.buffer.text(), "one two three\n", "toggled, not set");
    // A text object — the word the cursor is on after the motion before.
    feed(&mut ed, "gUiw");
    assert_eq!(ed.buffer.text(), "one TWO three\n");
    // The doubled key: whole lines, as many as the count says.
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "2gUU");
    assert_eq!(ed.buffer.text(), "ONE\nTWO\nthree\n");
    // `g~~` doubles the operator's own key, so it acts on the line the
    // cursor is on — which is the first one, where `gUU` left it.
    feed(&mut ed, "g~~");
    assert_eq!(ed.buffer.text(), "one\nTWO\nthree\n", "that line only");
    // A linewise motion: two lines, and their terminators are left alone.
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "gUj");
    assert_eq!(ed.buffer.text(), "ONE\nTWO\n", "no swallowed line break");
}

#[test]
fn gq_rewraps_the_lines_a_motion_names() {
    let mut ed = Editor::for_tests();
    ed.options.borrow_mut().textwidth = crate::settings::Count(20);
    ed.buffer = Buffer::from_text("one two three four\nfive six seven\neight nine ten\n");
    // The three lines are one paragraph, and twenty columns is three lines.
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "gqG");
    assert_eq!(
        ed.buffer.text(),
        "one two three four\nfive six seven eight\nnine ten\n"
    );
    assert!(ed.message.contains("rewrapped"), "{}", ed.message);
    // Short lines are joined, and one `u` puts the whole thing back.
    ed.buffer = Buffer::from_text("a\nb\nc\n");
    feed(&mut ed, "gqG");
    assert_eq!(ed.buffer.text(), "a b c\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "a\nb\nc\n", "one edit, one undo");
    // A word longer than the width is left on a line of its own.
    ed.buffer = Buffer::from_text("short aaaaaaaaaaaaaaaaaaaaaaa end\n");
    feed(&mut ed, "gqgq");
    let text = ed.buffer.text();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.iter().any(|line| line.starts_with("aaaa")),
        "{text:?}"
    );
    assert!(
        lines.iter().all(|line| line.chars().count() <= 25),
        "{text:?}"
    );
    // A width of zero is not a width: the command does nothing.
    ed.options.borrow_mut().textwidth = crate::settings::Count(0);
    ed.buffer = Buffer::from_text("one two three four five six seven eight nine ten\n");
    feed(&mut ed, "gqgq");
    assert_eq!(
        ed.buffer.text(),
        "one two three four five six seven eight nine ten\n"
    );
}

#[test]
fn screen_motions_go_to_the_lines_that_are_on_screen() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&"x\n".repeat(400));
    ed.viewport = crate::screen::Viewport::new(80, 24);
    ed.viewport.top_line = 100;
    ed.cursor.row = 150;
    ed.cursor.col = 3;
    // The rows the screen shows are 100 to 121, and the three motions are
    // its first, middle and last row.
    feed(&mut ed, "H");
    assert_eq!((ed.cursor.row, ed.cursor.col), (100, 0), "the top row");
    feed(&mut ed, "M");
    assert_eq!(ed.cursor.row, 111, "the middle one");
    feed(&mut ed, "L");
    assert_eq!(ed.cursor.row, 121, "and the bottom one");
    // A count counts from the edge it is measured from, as in vim: `3H` is
    // the third row down and `3L` the third row up.
    feed(&mut ed, "3H");
    assert_eq!(ed.cursor.row, 102);
    feed(&mut ed, "3L");
    assert_eq!(ed.cursor.row, 119);
    // The cursor lands on the text, not in the indentation.
    ed.buffer = Buffer::from_text(&"x\n".repeat(200));
    ed.buffer = Buffer::from_text(&format!("{}    indented\n", "x\n".repeat(10)));
    ed.viewport.top_line = 10;
    ed.cursor.row = 0;
    feed(&mut ed, "H");
    assert_eq!(ed.cursor.row, 10);
    assert_eq!(ed.cursor.col, 4, "past the indentation");
    // Past the end of the file the clamp wins: the last line is the last
    // line, whatever the screen would have shown.
    ed.buffer = Buffer::from_text(&"x\n".repeat(200));
    ed.cursor.row = 199;
    ed.after_edit();
    ed.viewport.top_line = 195;
    feed(&mut ed, "L");
    assert_eq!(ed.cursor.row, 199);
}

#[test]
fn a_screen_motion_is_a_linewise_one() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&(0..40).map(|n| format!("line {n}\n")).collect::<String>());
    ed.viewport = crate::screen::Viewport::new(80, 24);
    ed.viewport.top_line = 10;
    ed.cursor.row = 20;
    ed.cursor.col = 0;
    // `dH` deletes whole lines, from the screen's top row to the cursor's —
    // which is what makes it a motion rather than a command that moves.
    feed(&mut ed, "dH");
    assert_eq!(ed.buffer.line_count(), 29, "eleven lines went");
    assert_eq!(ed.buffer.line_string(9), "line 9");
    assert_eq!(
        ed.buffer.line_string(10),
        "line 21",
        "up to the cursor's line"
    );
}

#[test]
fn scrolloff_keeps_context_above_and_below_the_cursor() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&"x\n".repeat(200));
    ed.viewport = crate::screen::Viewport::new(80, 24);
    ed.options.borrow_mut().scrolloff = crate::settings::Count(3);
    // Moving up to the top row scrolls the view *before* the cursor reaches
    // the edge, so three lines stay above it.
    ed.cursor.row = 50;
    ed.after_edit();
    for _ in 0..20 {
        feed(&mut ed, "k");
    }
    assert_eq!(ed.cursor.row, 30);
    assert_eq!(ed.viewport.top_line, 27, "three lines of context above");
    // And the same below: the cursor stops three rows short of the bottom.
    for _ in 0..40 {
        feed(&mut ed, "j");
    }
    let rows = ed.viewport.text_rows() as usize;
    assert_eq!(ed.cursor.row, 70);
    assert_eq!(
        ed.viewport.top_line + rows - 1,
        73,
        "three lines of context below the cursor's own row"
    );
    // At the start and the end of the file there is no context to keep, and
    // the ordinary clamps win.
    ed.viewport.top_line = 0;
    ed.cursor.row = 0;
    ed.after_edit();
    assert_eq!(ed.viewport.top_line, 0);
    ed.cursor.row = 199;
    ed.after_edit();
    assert_eq!(ed.viewport.top_line, 199 + 1 - rows, "the end of the file");
    // Zero is the default and means the cursor may sit on the edge.
    ed.options.borrow_mut().scrolloff = crate::settings::Count(0);
    ed.cursor.row = 30;
    ed.after_edit();
    ed.viewport.top_line = 30;
    ed.cursor.row = 30;
    ed.after_edit();
    assert_eq!(ed.viewport.top_line, 30, "the cursor is the top row");
}

#[test]
fn scrolling_the_view_takes_the_cursor_with_it() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&"x\n".repeat(200));
    ed.viewport = crate::screen::Viewport::new(80, 24);
    ed.viewport.top_line = 10;
    ed.cursor.row = 20;
    // Vim's rule: the text and the cursor move together, so the cursor keeps
    // its row on the screen — and the view is what moved.
    feed(&mut ed, "<C-e>");
    assert_eq!(ed.viewport.top_line, 11, "the view moved");
    assert_eq!(ed.cursor.row, 21, "and the cursor with it");
    // A count scrolls that many lines.
    feed(&mut ed, "5<C-e>");
    assert_eq!(ed.viewport.top_line, 16);
    assert_eq!(ed.cursor.row, 26);
    feed(&mut ed, "6<C-y>");
    assert_eq!(ed.viewport.top_line, 10, "and back up");
    assert_eq!(ed.cursor.row, 20);
    // At the top of the file the view cannot move, so neither does the
    // cursor: nothing scrolls, and the cursor is not pushed off the edge.
    feed(&mut ed, "25<C-y>");
    assert_eq!(ed.viewport.top_line, 0);
    assert_eq!(ed.cursor.row, 10, "ten lines, which is as far as it went");
    // At the end of the file the same, from the other side.
    ed.viewport.top_line = 178;
    ed.cursor.row = 199;
    feed(&mut ed, "25<C-e>");
    assert_eq!(ed.viewport.top_line, 178, "the last screenful stays put");
    assert_eq!(ed.cursor.row, 199);
    // `scrolloff` is kept even here: a scroll that would leave the cursor on
    // the top row stops it that many lines short instead.
    ed.options.borrow_mut().scrolloff = crate::settings::Count(3);
    ed.viewport.top_line = 30;
    ed.cursor.row = 32;
    feed(&mut ed, "<C-e>");
    assert_eq!(ed.viewport.top_line, 31);
    assert_eq!(ed.cursor.row, 34, "three lines below the top row");
}

#[test]
fn half_pages_and_pages_move_the_cursor_by_a_screen() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&"x\n".repeat(200));
    ed.viewport = crate::screen::Viewport::new(80, 24);
    let rows = ed.viewport.text_rows() as usize;
    assert_eq!(rows, 22, "the screen this test is about");
    ed.cursor.row = 100;
    ed.after_edit();
    // `<C-d>` is half a screen, `<C-f>` a whole one.
    feed(&mut ed, "<C-d>");
    assert_eq!(ed.cursor.row, 111, "half of 22");
    feed(&mut ed, "<C-f>");
    assert_eq!(ed.cursor.row, 132, "and then a whole one");
    feed(&mut ed, "<C-u>");
    assert_eq!(ed.cursor.row, 121, "half a page back up");
    feed(&mut ed, "<C-u>");
    assert_eq!(ed.cursor.row, 110, "and half of one again");
    // A count multiplies the step, and the ends of the file clamp it.
    feed(&mut ed, "2<C-d>");
    assert_eq!(ed.cursor.row, 132, "two half pages");
    feed(&mut ed, "99<C-d>");
    assert_eq!(ed.cursor.row, 199, "the last line");
    feed(&mut ed, "99<C-u>");
    assert_eq!(ed.cursor.row, 0, "the first");
}

#[test]
fn scrolling_the_view_is_not_a_motion() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    ed.viewport = crate::screen::Viewport::new(80, 24);
    feed(&mut ed, "d<C-e>");
    assert_eq!(
        ed.buffer.text(),
        "one\ntwo\nthree\n",
        "`d` followed by a scroll deletes nothing"
    );
    // And the half-typed operator is *not* left waiting: the scroll clears
    // it, so the next key is a key of its own rather than a motion.
    assert!(ed.pending.is_empty(), "no operator is left pending");
    feed(&mut ed, "j");
    assert_eq!(
        ed.buffer.text(),
        "one\ntwo\nthree\n",
        "and `j` is only a move"
    );
    assert_eq!(ed.cursor.row, 1);
}

#[test]
fn a_counted_operator_multiplies_the_motion_count() {
    // `2d3w` deletes six words — the counts multiply — and the same holds for
    // a forced shape: `2d<C-v>j` is two rows down.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two three four five six\n");
    ed.cursor.row = 0;
    ed.cursor.col = 0;
    feed(&mut ed, "2d3w");
    // Six `w`s from the start reach the last character of the last word, and
    // `w` is exclusive — so all but that character goes.
    assert_eq!(
        ed.buffer.text(),
        "x\n",
        "six words, less the last character"
    );
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\ntwo six\nsix ten\nfour five\n");
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "2d<C-v>j");
    assert_eq!(
        ed.buffer.text(),
        "one wo\ntwo ix\nsix en\nfour five\n",
        "one column, three rows"
    );
}

#[test]
fn a_count_repeats_the_command() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abcdef\n");
    feed(&mut ed, "3x");
    assert_eq!(ed.buffer.text(), "def\n");
    feed(&mut ed, "2dd");
    assert_eq!(ed.buffer.text(), "\n", "and a count works on a line too");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&"line\n".repeat(9));
    feed(&mut ed, "5j");
    assert_eq!(ed.cursor.row, 5);
    feed(&mut ed, "0");
    assert_eq!(ed.cursor.col, 0, "a leading zero is still a motion");
    feed(&mut ed, "100j");
    assert_eq!(ed.cursor.row, 8, "a count past the end stops at the end");
}

#[test]
fn a_count_stops_at_the_end_of_the_line() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abc\ndef\n");
    feed(&mut ed, "9x");
    assert_eq!(
        ed.buffer.text(),
        "\ndef\n",
        "`9x` takes the rest of the line, and does not join the next one"
    );
}

#[test]
fn the_delete_operator_takes_a_motion() {
    for (keys, before, after) in [
        ("dw", "one two three\n", "two three\n"),
        ("d$", "one two\n", "\n"),
        ("dl", "one\n", "ne\n"),
        ("dG", "one\ntwo\nthree\n", "\n"),
    ] {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(before);
        feed(&mut ed, keys);
        assert_eq!(ed.buffer.text(), after, "{keys} on {before:?}");
    }

    // Motions that start from a column, and the ones that cover lines.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    ed.cursor = Cursor::new(0, 4);
    feed(&mut ed, "d0");
    assert_eq!(ed.buffer.text(), "two\n");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    ed.cursor = Cursor::new(0, 4);
    feed(&mut ed, "db");
    assert_eq!(ed.buffer.text(), "two\n");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    feed(&mut ed, "dj");
    assert_eq!(ed.buffer.text(), "three\n", "`dj` takes whole lines");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    ed.cursor = Cursor::new(2, 0);
    feed(&mut ed, "dk");
    assert_eq!(ed.buffer.text(), "one\n");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.cursor = Cursor::new(1, 1);
    feed(&mut ed, "dh");
    assert_eq!(ed.buffer.text(), "one\nwo\n", "backwards takes one char");

    // `dgg` from the middle: the motion is multi-key, and the operator waits
    // for the second `g`.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    ed.cursor = Cursor::new(2, 0);
    feed(&mut ed, "dgg");
    assert_eq!(ed.buffer.text(), "\n");

    // ...and from the first line it takes only that one.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    feed(&mut ed, "dgg");
    assert_eq!(ed.buffer.text(), "two\nthree\n");
}

#[test]
fn counts_multiply_across_the_operator_and_its_motion() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("a b c d e f g h\n");
    feed(&mut ed, "2d3w");
    assert_eq!(ed.buffer.text(), "g h\n", "two times three words is six");
}

#[test]
fn an_operator_that_never_gets_a_motion_leaves_nothing_behind() {
    // `d` then `i` asks for a text object, and waits for it.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "di");
    assert_eq!(ed.mode, Mode::Normal, "`di` waits, it is not insert mode");
    assert_eq!(ed.buffer.text(), "one\n");
    assert_eq!(ed.operator, Some(Operator::Delete), "still waiting");
    assert_eq!(ed.object, Some(Object::Inner));
    // Escape is not an object and leaves nothing behind.
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.operator, None);
    assert_eq!(ed.object, None);

    // A key that is neither a motion nor an object's name beeps. `q` is not
    // one — it is the doubled half of `gq`, so it only means something after
    // a `g` — and neither is `<F12>`.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "d<Space>");
    assert_eq!(ed.message, "unmapped key: <Space>");
    assert_eq!(ed.operator, None, "and the operator went with it");

    // (A key that only *starts* a binding is not a key either, and it waits
    // rather than ending the operator: `dg` is the start of `dgg`.)
    let mut ed = Editor::for_tests();
    feed(&mut ed, "dg");
    assert_eq!(ed.operator, Some(Operator::Delete), "still waiting");
    assert_eq!(ed.message, "", "and nothing has been said yet");

    // And a half-typed command does not survive the mode changing.
    let mut ed = Editor::for_tests();
    feed(&mut ed, "d<Esc>");
    assert_eq!(ed.operator, None);
    assert_eq!(ed.pending_count, "");
}

#[test]
fn an_operator_takes_a_count_of_whole_lines() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
    feed(&mut ed, "2dd");
    assert_eq!(ed.buffer.text(), "three\nfour\n");
    feed(&mut ed, "G2yy");
    assert_eq!(
        ed.buffer.text(),
        "three\nfour\n",
        "yanking still changes nothing"
    );
    feed(&mut ed, "p");
    assert_eq!(
        ed.buffer.text(),
        "three\nfour\nthree\nfour\n",
        "two lines went in below the last one"
    );
}

#[test]
fn character_finds_move_along_the_line() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one, two, three\n");
    feed(&mut ed, "f,");
    assert_eq!(ed.cursor.col, 3, "the next comma");
    feed(&mut ed, ";");
    assert_eq!(ed.cursor.col, 8, "the one after it");
    feed(&mut ed, ",");
    assert_eq!(ed.cursor.col, 3, "and back the other way");
    feed(&mut ed, "t,");
    assert_eq!(ed.cursor.col, 7, "`t` stops just before it");
    feed(&mut ed, "T,");
    assert_eq!(ed.cursor.col, 4, "and `T` just after it, going back");

    // A count passes that many of them.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("a.b.c.d\n");
    feed(&mut ed, "2f.");
    assert_eq!(ed.cursor.col, 3);
    // A character that is not on the line leaves the cursor alone.
    feed(&mut ed, "fz");
    assert_eq!(ed.cursor.col, 3);

    // `F` looks back, and `Escape` calls a find off.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one, two, three\n");
    feed(&mut ed, "$F,");
    assert_eq!(ed.cursor.col, 8);
    feed(&mut ed, "f");
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.pending, "", "the half-typed find is called off");
    assert_eq!(ed.cursor.col, 8, "and nothing moved");
}

#[test]
fn a_find_is_a_motion_for_the_operators() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one, two, three\n");
    feed(&mut ed, "df,");
    assert_eq!(ed.buffer.text(), " two, three\n", "through the comma");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("call(one, two)\n");
    feed(&mut ed, "dt)");
    assert_eq!(ed.buffer.text(), ")\n", "up to the bracket, not through it");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("let x = \"quoted\";\n");
    feed(&mut ed, "yf\"");
    assert_eq!(ed.register.text(), "let x = \"", "yanked through the quote");

    // In a selection a find moves the end of the range.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one, two\n");
    feed(&mut ed, "vf,");
    assert_eq!(ed.mode, Mode::Visual);
    feed(&mut ed, "d");
    assert_eq!(ed.buffer.text(), " two\n");
}

#[test]
fn a_count_with_g_is_a_line_number() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&"line\n".repeat(200));
    feed(&mut ed, "5G");
    assert_eq!(ed.cursor.row, 4, "the fifth line");
    feed(&mut ed, "300G");
    assert_eq!(ed.cursor.row, 199, "past the end is the end");
    feed(&mut ed, "G");
    assert_eq!(ed.cursor.row, 199, "and no count at all is the last line");
}

#[test]
fn the_window_can_be_scrolled_around_the_cursor() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&"line\n".repeat(200));
    ed.viewport.width = 40;
    ed.viewport.height = 12; // ten rows of text
    feed(&mut ed, "G");
    assert_eq!(ed.cursor.row, 199);

    // The cursor does not move: only the window does.
    feed(&mut ed, "zz");
    assert_eq!(ed.cursor.row, 199);
    assert_eq!(ed.viewport.top_line, 194, "five rows above the middle");
    feed(&mut ed, "zt");
    assert_eq!(ed.viewport.top_line, 199, "at the top");
    feed(&mut ed, "zb");
    assert_eq!(ed.viewport.top_line, 190, "and at the bottom");

    // Near the top of the buffer there is nothing above to show.
    feed(&mut ed, "ggzz");
    assert_eq!(ed.viewport.top_line, 0);
    assert_eq!(ed.cursor.row, 0);

    // And every one of them is still a scroll: the cursor stays where it is
    // and the text under it does not change.
    feed(&mut ed, "300G"); // past the end: the last line, as `G` does
    assert_eq!(ed.cursor.row, 199);
}

#[test]
fn debug_scroll() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&"line\n".repeat(200));
    ed.viewport.width = 40;
    ed.viewport.height = 12;
    feed(&mut ed, "50G");
    println!(
        "after 50G: row={} top={}",
        ed.cursor.row, ed.viewport.top_line
    );
    feed(&mut ed, "zz");
    println!(
        "after zz: row={} top={}",
        ed.cursor.row, ed.viewport.top_line
    );
}

#[test]
fn windows_tile_the_screen_and_close_back_down() {
    let mut ed = Editor::for_tests();
    ed.viewport.width = 80;
    ed.viewport.height = 24;
    ed.split_window(Axis::Columns, None);
    ed.split_window(Axis::Rows, None);
    assert_eq!(ed.windows.len(), 3);

    let rects = ed.window_rects();
    // Two columns: the left one and the right one, with a separator column
    // between them.
    assert_eq!(rects[0].x, 0);
    assert_eq!(rects[0].y, 0);
    assert_eq!(rects[1].x, rects[0].width + 1);
    assert_eq!(rects[0].height, 23); // the message row is not a window's
                                     // And the right column is split into two rows.
    assert_eq!(
        rects[1].height + rects[2].height,
        23,
        "no separator row between them"
    );
    assert_eq!(rects[1].y, 0);
    assert_eq!(
        rects[2].y, rects[1].height,
        "the row between belongs to the window above"
    );
    // Every rectangle is inside the screen and they do not overlap.
    for (index, rect) in rects.iter().enumerate() {
        assert!(rect.x + rect.width <= 80, "{index}");
        assert!(rect.y + rect.height <= 23, "{index}");
    }

    // Closing the windows leaves the last one, and the layout with it.
    ed.close_window(2);
    assert_eq!(ed.windows.len(), 2);
    assert_eq!(ed.window_rects()[1].height, 23, "the split collapsed");
    ed.close_window(1);
    assert_eq!(ed.windows.len(), 1);
    assert_eq!(ed.window_rects()[0].width, 80);
    assert_eq!(ed.layout, Layout::Leaf(0));
}

#[test]
fn resize_scrolls_the_cursor_into_view() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&"line\n".repeat(50));
    ed.cursor = Cursor::new(49, 0);
    ed.viewport = Viewport::new(80, 10);
    ed.handle_resize(80, 10);
    assert_eq!(ed.viewport.text_rows(), 8);
    assert!(ed.viewport.top_line > 0);
    assert!(ed.cursor.row < ed.viewport.top_line + 8);
}

#[test]
fn page_keys_scroll_a_screenful() {
    let mut ed = Editor::for_tests();
    ed.handle_resize(20, 10); // 8 text rows
    ed.buffer = Buffer::from_text(&"line\n".repeat(30));
    ed.cursor = Cursor::new(0, 0);

    feed(&mut ed, "<PageDown>");
    assert_eq!(ed.cursor.row, 7, "one screen minus a line of overlap");
    feed(&mut ed, "<PageDown>");
    assert_eq!(ed.cursor.row, 14);
    feed(&mut ed, "<PageUp>");
    assert_eq!(ed.cursor.row, 7);
    feed(&mut ed, "<PageDown><PageDown>");
    assert_eq!(ed.cursor.row, 21);
    feed(&mut ed, "<PageDown>");
    assert_eq!(ed.cursor.row, 28, "clamped to the last line");
}

#[test]
fn horizontal_scroll_accounts_for_the_line_number_gutter() {
    let mut ed = Editor::for_tests();
    ed.options.borrow_mut().number = true;
    ed.handle_resize(20, 4);
    ed.buffer = Buffer::from_text(&"0123456789".repeat(10));
    ed.cursor = Cursor::new(0, 99);
    ed.after_edit();

    let out = ed.compose();
    assert!(out.cursor_visible);
    assert_eq!(out.cursor, (19, 0), "the cursor is in the last column");
    assert_eq!(
        out.grid.get(19, 0).text,
        "9",
        "the last column shows the character under the cursor"
    );
    assert_eq!(
        out.grid.get(2, 0).text,
        "2",
        "2 gutter columns, then col 82"
    );
}

#[test]
fn horizontal_scroll_without_a_gutter_uses_the_whole_width() {
    let mut ed = Editor::for_tests();
    ed.handle_resize(20, 4);
    ed.buffer = Buffer::from_text(&"0123456789".repeat(10));
    ed.cursor = Cursor::new(0, 99);
    ed.after_edit();

    let out = ed.compose();
    assert_eq!(out.cursor, (19, 0));
    assert_eq!(out.grid.get(19, 0).text, "9");
}
