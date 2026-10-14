use super::*;

#[test]
fn folds_hide_lines_and_the_keys_open_them_again() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&(0..20).map(|n| format!("line {n}\n")).collect::<String>());
    ed.viewport = crate::screen::Viewport::new(80, 12);
    ed.cursor.row = 0;
    // `zf` with a motion folds the lines it covers, and the fold starts closed.
    feed(&mut ed, "zf5j");
    assert_eq!(ed.folds.len(), 1, "one fold");
    let fold = ed.folds[0];
    assert_eq!((fold.start, fold.end), (0, 5));
    assert!(fold.closed);
    assert!(ed.line_is_folded(3), "its lines are hidden");
    assert!(!ed.line_is_folded(0), "except the first, which is drawn");
    assert!(!ed.line_is_folded(6), "and the one after it is not in it");
    // A closed fold is one line as far as a motion is concerned: `j` from its
    // first line steps over the whole thing, which is what makes the fold
    // something a person can move through rather than into.
    feed(&mut ed, "3j");
    assert_eq!(ed.cursor.row, 6, "the line after the fold");
    feed(&mut ed, "k");
    assert_eq!(ed.cursor.row, 0, "and back up onto its first line");
    // Moving *up* into one lands on its first line, since that is the line that
    // is drawn.
    feed(&mut ed, "k");
    assert_eq!(
        ed.cursor.row, 0,
        "up into the fold, and onto its first line"
    );
    // `zo` opens it and the lines come back; `zc` closes it again.
    feed(&mut ed, "zo");
    assert!(!ed.folds[0].closed);
    feed(&mut ed, "3j");
    assert_eq!(ed.cursor.row, 3, "and now it can go in");
    feed(&mut ed, "zc");
    assert!(ed.folds[0].closed);
    assert_eq!(ed.cursor.row, 0, "back to the top of it");
    // `zR` and `zM` do all of them.
    feed(&mut ed, "zo");
    ed.cursor.row = 10;
    feed(&mut ed, "zf3j");
    assert_eq!(ed.folds.len(), 2);
    // `zj` and `zk` step to the start of the next and previous fold, and a
    // count takes that many steps.
    ed.cursor.row = 0;
    feed(&mut ed, "zj");
    assert_eq!(ed.cursor.row, 10, "the next fold's first line");
    feed(&mut ed, "zk");
    assert_eq!(ed.cursor.row, 0, "and back to the one before it");
    feed(&mut ed, "2zj");
    assert_eq!(
        ed.cursor.row, 10,
        "a count steps twice, and stops at the last"
    );
    ed.cursor.row = 0;
    feed(&mut ed, "zk");
    assert!(
        ed.message.starts_with("E490"),
        "nothing before it: {}",
        ed.message
    );

    feed(&mut ed, "zM");
    assert!(ed.folds.iter().all(|fold| fold.closed), "all closed");
    feed(&mut ed, "zR");
    assert!(ed.folds.iter().all(|fold| !fold.closed), "all open");
    // `zd` takes the one the cursor is in away.
    ed.cursor.row = 0;
    feed(&mut ed, "zd");
    assert_eq!(ed.folds.len(), 1, "the first one went");
    assert_eq!(ed.folds[0].start, 10, "and the other is still there");
    // `zf` with no motion waits for one — it is an operator — and a motion
    // that covers one line is not a fold: there would be nothing to hide.
    ed.cursor.row = 0;
    feed(&mut ed, "zfl");
    assert!(
        ed.message.contains("one line"),
        "a one-line motion has nothing to hide: {}",
        ed.message
    );
}

#[test]
fn a_fold_follows_the_lines_it_was_put_on() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("a\nb\nc\nd\ne\nf\n");
    ed.cursor.row = 2;
    // Three lines: `c`, `d` and `e`.
    feed(&mut ed, "zf2j");
    assert_eq!((ed.folds[0].start, ed.folds[0].end), (2, 4));
    assert_eq!(ed.buffer.line_string(ed.folds[0].start), "c");

    // A line added *above* the fold moves it down, so it still holds `c` —
    // which is the whole point of keeping the numbers up to date.
    ed.cursor.row = 0;
    feed(&mut ed, "o");
    feed(&mut ed, "new<Esc>");
    assert_eq!(ed.buffer.text(), "a\nnew\nb\nc\nd\ne\nf\n");
    assert_eq!(
        (ed.folds[0].start, ed.folds[0].end),
        (3, 5),
        "the fold moved with its text"
    );
    assert_eq!(ed.buffer.line_string(ed.folds[0].start), "c", "still `c`");

    // A line added *inside* the fold makes it bigger. The fold is opened first,
    // since a closed one hides the line the edit is on.
    ed.cursor.row = 3;
    feed(&mut ed, "zo");
    ed.cursor.row = 4;
    feed(&mut ed, "o");
    feed(&mut ed, "inside<Esc>");
    assert_eq!(
        (ed.folds[0].start, ed.folds[0].end),
        (3, 6),
        "the fold grew"
    );

    // And a line taken out of it makes it smaller again: `o` put `inside` below
    // the line the cursor was on, so it is that line the fold grew by.
    assert_eq!(ed.buffer.text(), "a\nnew\nb\nc\nd\ninside\ne\nf\n");
    ed.cursor.row = 5;
    feed(&mut ed, "dd");
    assert_eq!((ed.folds[0].start, ed.folds[0].end), (3, 5), "and shrank");
    assert_eq!(ed.buffer.text(), "a\nnew\nb\nc\nd\ne\nf\n", "`inside` went");
}

#[test]
fn an_operator_takes_a_closed_fold_as_a_whole() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\nfive\n");
    ed.cursor.row = 1;
    feed(&mut ed, "zf2j");
    assert_eq!(
        (ed.folds[0].start, ed.folds[0].end),
        (1, 3),
        "lines two to four"
    );
    // `dl` on the fold's first line deletes the whole fold — the manual's own
    // example — rather than the one character the motion covers.
    feed(&mut ed, "dl");
    assert_eq!(ed.buffer.text(), "one\nfive\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\ntwo\nthree\nfour\nfive\n", "one `u`");

    // A linewise motion from the line *above* a closed fold goes further than the
    // count says, and that is the stepping rule rather than the widening: `j` onto
    // a closed fold's first line is one row, and the next `j` from there steps
    // over the whole fold to the line after it — so `d2j` from row 0 covers rows 0
    // to 4. (A fresh editor, because the `u` above puts the *text* back and not
    // the fold: a fold is not part of the history, which is on the roadmap.)
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\nfive\n");
    ed.cursor.row = 1;
    feed(&mut ed, "zf2j");
    ed.cursor.row = 0;
    feed(&mut ed, "d2j");
    assert_eq!(
        ed.buffer.text(),
        "\n",
        "the line above and the fold, and past it"
    );

    // An open fold is not a whole: `dl` is one character again.
    feed(&mut ed, "u");
    ed.cursor.row = 1;
    feed(&mut ed, "zo");
    feed(&mut ed, "dl");
    assert_eq!(ed.buffer.text(), "one\nwo\nthree\nfour\nfive\n");
}

#[test]
fn an_undo_takes_a_fold_back_with_the_text_it_was_on() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\nfive\n");
    ed.cursor.row = 1;
    feed(&mut ed, "zf2j");
    assert_eq!((ed.folds[0].start, ed.folds[0].end), (1, 3));
    // The fold's lines go, and the fold with them.
    feed(&mut ed, "dl");
    assert_eq!(ed.buffer.text(), "one\nfive\n");
    assert!(ed.folds.is_empty(), "nothing left to fold");
    // One `u` puts the text back *and* the fold, which is what vim does and what
    // this did not: a fold is not text, so it is not in the history — the editor
    // keeps a second stack in step with it.
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\ntwo\nthree\nfour\nfive\n");
    assert_eq!(ed.folds.len(), 1, "the fold came back");
    assert_eq!((ed.folds[0].start, ed.folds[0].end), (1, 3), "where it was");
    // And `redo` takes it away again, with the text: the entry a redo lands on is
    // the state the undone step left, which the edit wrote after itself.
    feed(&mut ed, "<C-r>");
    assert_eq!(ed.buffer.text(), "one\nfive\n");
    assert!(ed.folds.is_empty(), "and away again");
}

#[test]
fn the_manual_s_behavior_of_folds_rules() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\nfive\n");
    ed.cursor.row = 1;
    feed(&mut ed, "zf2j");
    assert_eq!((ed.folds[0].start, ed.folds[0].end), (1, 3));

    // Closing a fold with the cursor inside it puts it on the fold's first line —
    // and its *column* is left alone, since "displayed in the first column" is the
    // drawing's business and not the cursor's value. (Moved with keys, since a
    // closed fold is one line to a motion and `j` would step over it.)
    feed(&mut ed, "zo");
    feed(&mut ed, "j");
    feed(&mut ed, "zc");
    assert_eq!(ed.cursor.row, 1, "the fold's first line");

    // A search from a closed fold starts *past* it, so a match inside the fold is
    // not found from there: forward from the line after it, backward from its
    // first line.
    ed.cursor.row = 1;
    ed.cursor.col = 0;
    assert_eq!(
        ed.search_start(crate::search::Direction::Forward),
        ed.buffer.line_col_to_char(4, 0),
        "forward starts past the fold"
    );
    assert_eq!(
        ed.search_start(crate::search::Direction::Backward),
        ed.buffer.line_col_to_char(1, 0),
        "backward starts at its first line"
    );

    // Insert mode never leaves the cursor on a folded line: it opens the fold, so
    // what is typed can be seen.
    ed.cursor.row = 1;
    assert!(ed.fold_is_shut(0));
    feed(&mut ed, "i");
    assert!(!ed.fold_is_shut(0), "the fold opened for typing");
    assert_eq!(ed.mode, Mode::Insert);
    feed(&mut ed, "<Esc>");
}

#[test]
fn a_substitute_range_is_adjusted_to_a_closed_fold() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("a1\nb1\nc1\nd1\ne1\n");
    ed.cursor.row = 1;
    feed(&mut ed, "zf2j");
    assert_eq!((ed.folds[0].start, ed.folds[0].end), (1, 3));
    // `:s` with no range runs over the cursor's line — and the cursor is on a
    // closed fold, so the range is the fold's. Vim's example is exactly this.
    ed.cursor.row = 1;
    ed.execute_cmdline("s/1/X/");
    assert_eq!(
        ed.buffer.text(),
        "a1\nbX\ncX\ndX\ne1\n",
        "every line of the fold"
    );
    // A range that reaches into the fold takes all of it too — and the lines it
    // named of its own, since the adjustment widens rather than replaces: `1,2` is
    // lines 1 and 2, and line 2 is inside the fold, so the fold comes with it.
    feed(&mut ed, "u");
    ed.execute_cmdline("1,2s/1/Y/");
    assert_eq!(
        ed.buffer.text(),
        "aY\nbY\ncY\ndY\ne1\n",
        "the lines named, and the fold they reached into"
    );
    // A line that is not in the fold is left alone — line 5, since the fold is
    // lines 2 to 4 by the numbers a person types.
    feed(&mut ed, "u");
    ed.execute_cmdline("5s/1/Z/");
    assert_eq!(ed.buffer.text(), "a1\nb1\nc1\nd1\neZ\n", "just that line");
    feed(&mut ed, "u");
    ed.execute_cmdline("%s/1/W/");
    assert_eq!(
        ed.buffer.text(),
        "aW\nbW\ncW\ndW\neW\n",
        "the whole file, as `%` says"
    );
}

#[test]
fn foldlevel_closes_and_opens_a_level_at_a_time() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("top\n    one\n        two\n        two again\nback\n");
    ed.execute_cmdline("set foldmethod=indent");
    // Two folds, the inner nested inside the outer: their depths are one and two.
    assert_eq!(ed.folds.len(), 2);
    assert_eq!(ed.fold_depth(0), 1, "the outer one");
    assert_eq!(ed.fold_depth(1), 2, "the inner one");
    // With no level asked for, both are shut: vim's default, and what `zf` and
    // `indent` folding both do.
    assert!(ed.fold_is_shut(0) && ed.fold_is_shut(1));
    assert_eq!(ed.visible_lines(0, 12), vec![0, 1, 4], "one line each");
    // `zr` shows one level more: the outer one opens and the inner stays shut,
    // since a fold deeper than the level is drawn closed.
    feed(&mut ed, "zr");
    assert_eq!(ed.options.borrow().foldlevel.0, 1);
    assert!(!ed.fold_is_shut(0), "the outer one is shown open");
    assert!(ed.fold_is_shut(1), "the inner one is still shut");
    assert_eq!(ed.visible_lines(0, 12), vec![0, 1, 2, 4]);
    // And again: the inner one opens too.
    feed(&mut ed, "zr");
    assert_eq!(ed.visible_lines(0, 12), (0..5).collect::<Vec<usize>>());
    // `zm` closes one level, and one more closes everything.
    feed(&mut ed, "zm");
    assert_eq!(ed.options.borrow().foldlevel.0, 1);
    assert!(ed.fold_is_shut(1), "the inner one shut again");
    feed(&mut ed, "zm");
    assert_eq!(ed.options.borrow().foldlevel.0, 0);
    assert!(ed.fold_is_shut(0) && ed.fold_is_shut(1), "both");
    assert_eq!(ed.visible_lines(0, 12), vec![0, 1, 4]);
    // `zR` opens every fold, level and all, and `zM` closes them.
    feed(&mut ed, "zR");
    assert_eq!(ed.visible_lines(0, 12), (0..5).collect::<Vec<usize>>());
    feed(&mut ed, "zM");
    assert_eq!(ed.visible_lines(0, 12), vec![0, 1, 4]);
}

#[test]
fn indent_folding_respects_foldignore_and_foldnestmax() {
    // A preprocessor line inside an indented block. The levels are the indent
    // divided by `'tabstop'`: 0 for `#include`, 1 for `body`, 0 for `# ifdef`, 2
    // for `deep`, and so on.
    let source = "#include <one>\n    body\n    body again\n# ifdef X\n        deep\n# endif\n    body once more\n";
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(source);
    // With nothing ignored, the `#` lines are ordinary lines at level zero, so the
    // block is cut in two — and only the first piece is more than one line.
    ed.execute_cmdline("set foldmethod=indent");
    assert_eq!(ed.folds.len(), 1, "the `#` lines cut the block short");
    assert_eq!((ed.folds[0].start, ed.folds[0].end), (1, 2), "`body` alone");

    // With `#` ignored, those lines take the lower of the levels around them and
    // the block is one fold again — from the `#include` line itself, since an
    // ignored line's level is what the fold is measured by.
    ed.execute_cmdline("set foldignore=#");
    assert_eq!(ed.folds.len(), 1, "one fold");
    assert_eq!(
        (ed.folds[0].start, ed.folds[0].end),
        (0, 6),
        "the whole block"
    );
    assert!(ed.line_is_folded(3), "the `# ifdef` line is inside it");
    assert!(ed.line_is_folded(4), "and so is `deep`");

    // `'foldnestmax'`: the level of a fold *is* its nesting, so a maximum of one
    // leaves the outer fold and not the inner one.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("top\n    one\n        two\n        two again\nback\n");
    ed.execute_cmdline("set foldmethod=indent");
    assert_eq!(ed.folds.len(), 2, "an outer fold and an inner one");
    ed.execute_cmdline("set foldnestmax=1");
    assert_eq!(ed.folds.len(), 1, "the inner one is too deep");
    assert_eq!(
        (ed.folds[0].start, ed.folds[0].end),
        (1, 3),
        "the outer one"
    );
    ed.execute_cmdline("set foldnestmax=20");
    assert_eq!(ed.folds.len(), 2, "and back again");
}

#[test]
fn a_view_keeps_the_folds_for_later() {
    let dir = std::env::temp_dir().join("inkstone-view");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("kept.txt");
    let lines: String = (0..20).map(|n| format!("line {n}\n")).collect();
    std::fs::write(&file, lines).unwrap();

    let mut ed = Editor::for_tests();
    ed.view_dir = Some(dir.join("views"));
    ed.open_file(&file).unwrap();
    ed.cursor.row = 2;
    feed(&mut ed, "zf3j");
    ed.cursor.row = 8;
    feed(&mut ed, "zf2j");
    feed(&mut ed, "zc");
    assert_eq!(ed.folds.len(), 2);
    let before = ed.folds.clone();
    ed.execute_cmdline("mkview");
    assert!(ed.message.starts_with("view written"), "{}", ed.message);
    assert!(ed.view_path().is_some_and(|path| path.is_file()));

    // The folds are forgotten, and the view brings them back — with the cursor
    // where it was when the view was made.
    let mut ed = Editor::for_tests();
    ed.view_dir = Some(dir.join("views"));
    ed.open_file(&file).unwrap();
    assert!(ed.folds.is_empty(), "a fresh session has none");
    ed.execute_cmdline("loadview");
    assert_eq!(ed.folds, before, "the folds came back");
    assert_eq!(ed.cursor.row, 8, "and so did the cursor");
    assert!(ed.message.contains("fold(s) restored"), "{}", ed.message);

    // A view that is not there says so rather than doing nothing.
    let elsewhere = dir.join("elsewhere.txt");
    std::fs::write(&elsewhere, "x\n").unwrap();
    ed.open_file(&elsewhere).unwrap();
    ed.execute_cmdline("loadview");
    assert!(ed.message.starts_with("no view for"), "{}", ed.message);
    assert!(ed.folds.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn indent_folding_works_the_folds_out_from_the_indent() {
    let mut ed = Editor::for_tests();
    // Two levels of nesting, a blank line inside the inner one, and a line back
    // at the top level to close both.
    ed.buffer =
        Buffer::from_text("top\n    one\n        two\n\n        two again\n    one again\nback\n");
    ed.execute_cmdline("set foldmethod=indent");
    assert!(ed.foldmethod_is_indent());
    assert_eq!(
        ed.folds.len(),
        2,
        "setting the method is what works them out"
    );
    assert_eq!(
        (ed.folds[0].start, ed.folds[0].end),
        (1, 5),
        "the outer one"
    );
    assert_eq!(
        (ed.folds[1].start, ed.folds[1].end),
        (2, 4),
        "the inner one"
    );
    // The blank line belongs to the fold around it: it is inside the inner one
    // and does not start anything.
    assert!(
        ed.line_is_folded(3),
        "the blank line is inside the inner fold"
    );
    assert_eq!(
        ed.visible_lines(0, 12),
        vec![0, 1, 6],
        "the outer fold is closed, so only its first line is drawn"
    );
    // Opening it shows the inner one's own lines gone: the inner fold is still
    // closed, which is what nesting does.
    ed.cursor.row = 1;
    feed(&mut ed, "zo");
    assert!(!ed.folds[0].closed);
    assert_eq!(
        ed.visible_lines(0, 12),
        vec![0, 1, 2, 5, 6],
        "the inner one"
    );
    ed.buffer = Buffer::from_text("a\nb\nc\n");
    ed.cursor.row = 0;
    feed(&mut ed, "zx");
    assert!(ed.folds.is_empty(), "nothing is indented");
    assert_eq!(ed.message, "0 folds");
    // In `manual` mode the folds are what a person made, and `zx` says so.
    ed.execute_cmdline("set foldmethod=manual");
    ed.buffer = Buffer::from_text("top\n    one\n        two\nback\n");
    feed(&mut ed, "zx");
    assert!(ed.folds.is_empty(), "manual mode computes nothing");
    assert!(ed.message.starts_with("E490"), "{}", ed.message);
    ed.cursor.row = 1;
    feed(&mut ed, "zf1j");
    assert_eq!(ed.folds.len(), 1, "but `zf` still works");
}

#[test]
fn folds_nest_inside_each_other() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(&(0..10).map(|n| format!("line {n}\n")).collect::<String>());
    ed.viewport = crate::screen::Viewport::new(80, 12);
    // The outer fold first, and opened: a closed fold is one line to a motion,
    // so making the inner one while the outer is closed would have the cursor
    // step over the outer instead of landing where the motion asked.
    ed.cursor.row = 1;
    feed(&mut ed, "zf6j");
    assert_eq!(
        (ed.folds[0].start, ed.folds[0].end),
        (1, 7),
        "the outer one"
    );
    feed(&mut ed, "zo");
    ed.cursor.row = 3;
    feed(&mut ed, "zf2j");
    assert_eq!(
        (ed.folds[1].start, ed.folds[1].end),
        (3, 5),
        "the inner one"
    );
    assert_eq!(ed.folds.len(), 2, "both are there, one inside the other");
    // Closing the outer one hides everything inside it, inner fold and all.
    ed.cursor.row = 1;
    feed(&mut ed, "zc");
    assert_eq!(
        ed.visible_lines(0, 12),
        vec![0, 1, 8, 9],
        "the outer fold is what is drawn"
    );
    // Opening it shows the inner one's own lines gone and the outer's others
    // back — which is what nesting means.
    feed(&mut ed, "zo");
    assert!(
        !ed.folds[0].closed && ed.folds[1].closed,
        "one opened, one not"
    );
    assert_eq!(
        ed.visible_lines(0, 12),
        vec![0, 1, 2, 3, 6, 7, 8, 9],
        "the inner fold's lines are still gone"
    );
    // The cursor inside both settles on the *innermost* fold's first line. (The
    // direction is an input here, since a motion that lands inside a closed fold
    // steps over it — that is what makes `j` a motion through a fold and not
    // into one — so this is the kernel's rule, tested where it is decided.)
    ed.last_cursor_row = 4;
    ed.cursor.row = 4;
    ed.settle_cursor();
    assert_eq!(ed.cursor.row, 3, "the innermost fold, not the outer one");
    // `zR` opens both, `zM` closes both.
    feed(&mut ed, "zR");
    assert!(ed.folds.iter().all(|fold| !fold.closed));
    assert_eq!(ed.visible_lines(0, 12), (0..10).collect::<Vec<usize>>());
    feed(&mut ed, "zM");
    assert!(ed.folds.iter().all(|fold| fold.closed));
    assert_eq!(ed.visible_lines(0, 12), vec![0, 1, 8, 9]);
    // A fold over exactly the same lines twice is one fold, not two — the kernel
    // rule, called directly, since a *motion* over a closed fold steps over it
    // and would not arrive where the fold is.
    let before = ed.folds.len();
    ed.fold_lines(2, 6);
    assert_eq!(
        ed.folds.len(),
        before + 1,
        "a different range is its own fold"
    );
    ed.fold_lines(2, 6);
    assert_eq!(
        ed.folds.len(),
        before + 1,
        "and the same range replaces itself"
    );
}

#[test]
fn a_closed_fold_is_drawn_as_one_line() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\nfive\n");
    ed.viewport = crate::screen::Viewport::new(40, 8);
    ed.cursor.row = 0;
    feed(&mut ed, "zf2j");
    // The window draws the fold's first line and then the line after it: the
    // rows and the lines are no longer the same thing.
    let visible = ed.visible_lines(0, 4);
    assert_eq!(visible, vec![0, 3, 4], "three rows for five lines");
    assert_eq!(ed.visible_rows_between(0, 3), 1, "one row between them");
    // The top of the view cannot be a line that is not drawn.
    ed.viewport.top_line = 2;
    ed.after_edit();
    assert!(
        !ed.line_is_folded(ed.viewport.top_line),
        "the top snapped out"
    );
}
