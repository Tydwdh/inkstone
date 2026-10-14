use super::*;

#[test]
fn a_case_change_over_a_selection() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one TWO\nthree\n");
    // `U` over a word: the selection, and back to normal mode.
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "viwU");
    assert_eq!(ed.buffer.text(), "ONE TWO\nthree\n");
    assert_eq!(ed.mode, Mode::Normal, "visual mode is over");
    // `u` is lower case here, not undo — which is what normal mode's is for.
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "viwu");
    assert_eq!(ed.buffer.text(), "one TWO\nthree\n");
    // `~` swaps every letter of the selection.
    ed.cursor = Cursor::new(0, 4);
    feed(&mut ed, "v3l~");
    assert_eq!(ed.buffer.text(), "one two\nthree\n", "TWO became two");
    // A linewise selection.
    feed(&mut ed, "VjgU");
    assert_eq!(ed.buffer.text(), "ONE TWO\nTHREE\n");
    // And the register is untouched: a case change is not a cut.
    ed.buffer = Buffer::from_text("one\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "yy");
    feed(&mut ed, "VgU");
    feed(&mut ed, "p");
    assert_eq!(ed.buffer.text(), "ONE\none\n", "the yank is still there");
}

#[test]
fn the_last_selection_and_the_last_insert_can_be_gone_back_to() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two three\nfour five\n");
    // Nothing to come back to yet.
    feed(&mut ed, "gv");
    assert!(ed.message.contains("E20"), "{}", ed.message);
    assert_eq!(ed.mode, Mode::Normal);
    // A selection, left, and come back to: `gv` is the same selection.
    ed.cursor = Cursor::new(0, 4);
    feed(&mut ed, "v2l");
    assert_eq!(ed.mode, Mode::Visual);
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.mode, Mode::Normal);
    assert_eq!(ed.selection, None);
    feed(&mut ed, "gv");
    assert_eq!(ed.mode, Mode::Visual);
    assert_eq!(ed.selection.map(|s| s.anchor), Some(Cursor::new(0, 4)));
    assert_eq!(ed.cursor, Cursor::new(0, 6));
    // And a selection that was *used* is remembered too.
    feed(&mut ed, "d");
    assert_eq!(
        ed.buffer.text(),
        "one  three\nfour five\n",
        "the `two` went"
    );
    feed(&mut ed, "gv");
    assert_eq!(ed.mode, Mode::Visual);
    assert_eq!(ed.selection.map(|s| s.anchor), Some(Cursor::new(0, 4)));
    feed(&mut ed, "<Esc>");
    // `gi` goes where insert mode was left and inserts there.
    ed.buffer = Buffer::from_text("one two\n");
    ed.cursor = Cursor::new(0, 3);
    feed(&mut ed, "iX<Esc>");
    assert_eq!(ed.buffer.text(), "oneX two\n");
    feed(&mut ed, "0");
    feed(&mut ed, "gi");
    assert_eq!(ed.mode, Mode::Insert, "inserting again");
    ed.type_text("Y");
    assert_eq!(ed.buffer.text(), "oneXY two\n", "at the same place");
    feed(&mut ed, "<Esc>");
    // With nothing remembered there is nothing to go back to.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "gi");
    assert!(ed.message.contains("E20"), "{}", ed.message);
    assert_eq!(ed.mode, Mode::Normal);
}

#[test]
fn a_selection_can_be_moved_a_level_and_reindented() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    // `>` over two lines moves both, and leaves visual mode.
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "vj<gt>");
    assert_eq!(ed.buffer.text(), "\tone\n\ttwo\nthree\n");
    assert_eq!(ed.mode, Mode::Normal);
    assert_eq!(ed.selection, None);
    // And `gv` comes back to what was moved, which is what makes the two
    // work together.
    feed(&mut ed, "gv");
    assert_eq!(ed.mode, Mode::Visual);
    assert_eq!(ed.selection.map(|s| s.anchor), Some(Cursor::new(0, 0)));
    feed(&mut ed, "<lt>");
    assert_eq!(ed.buffer.text(), "one\ntwo\nthree\n", "and back out again");
    assert_eq!(ed.mode, Mode::Normal);
    // `=` over a selection reindents it.
    ed.buffer = Buffer::from_text("fn main() {\nlet x = 1;\n}\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "Vj=");
    assert_eq!(ed.buffer.text(), "fn main() {\n\tlet x = 1;\n}\n");
    assert_eq!(ed.mode, Mode::Normal);
    // A linewise selection of the whole file, which is what `gg=G` is.
    ed.buffer = Buffer::from_text("a\n{\nb\n}\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "V3j=");
    assert_eq!(ed.buffer.text(), "a\n{\n\tb\n}\n");
}

#[test]
fn a_block_selection_is_a_rectangle() {
    let mut ed = Editor::for_tests();
    // The rows are the same width up to the block, so the rectangle is
    // what a person would see: `two`, `four`, `six` under each other.
    ed.buffer = Buffer::from_text("one two\nsix four\nten six\nab\n");
    // `<C-v>` starts one, and the kind is what the status line shows.
    feed(&mut ed, "<C-v>");
    assert_eq!(
        ed.selection.map(|selection| selection.kind),
        Some(VisualKind::Blockwise)
    );
    assert_eq!(ed.mode, Mode::Visual);
    assert_eq!(VisualKind::Blockwise.name(), " BLOCK");
    // From (0,4) to (2,6): three rows, three columns. (`<C-v>` was pressed
    // once above; pressing it again would put the selection away.)
    ed.selection = Some(Selection {
        anchor: Cursor::new(0, 4),
        kind: VisualKind::Blockwise,
        to_end: false,
    });
    ed.cursor.row = 2;
    ed.cursor.col = 6;
    assert_eq!(ed.visual_block(), Some((0, 2, 4, Some(6))));
    // The spans are the same columns on each row, in character offsets.
    assert_eq!(
        ed.block_spans()
            .iter()
            .map(|(start, end)| ed.buffer.slice_text(*start, *end))
            .collect::<Vec<_>>(),
        vec!["two", "fou", "six"]
    );
    // A row shorter than the rectangle contributes only what it has: the
    // last row is `ab`, two characters, and the block starts at 4.
    ed.cursor.row = 3;
    ed.cursor.col = 6;
    assert_eq!(ed.visual_block(), Some((0, 3, 4, Some(6))));
    assert_eq!(
        ed.block_spans()
            .iter()
            .map(|(start, end)| ed.buffer.slice_text(*start, *end))
            .collect::<Vec<_>>(),
        vec!["two", "fou", "six", ""]
    );
    // An anchor below and right of the cursor is the same rectangle.
    ed.cursor.row = 0;
    ed.cursor.col = 0;
    ed.selection = Some(Selection {
        anchor: Cursor::new(3, 6),
        kind: VisualKind::Blockwise,
        to_end: false,
    });
    assert_eq!(ed.visual_block(), Some((0, 3, 0, Some(6))));
    // And a selection of another kind has no block.
    ed.selection = Some(Selection {
        anchor: Cursor::new(0, 0),
        kind: VisualKind::Charwise,
        to_end: false,
    });
    assert_eq!(ed.visual_block(), None);
    assert!(ed.block_spans().is_empty());
}

#[test]
fn a_block_can_be_deleted_yanked_and_changed() {
    let source = "one two\nsix four\nten six\n";
    // `d`: the rectangle's columns go from every row, in one undo step.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(source);
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "<C-v>");
    ed.cursor.row = 2;
    ed.cursor.col = 6;
    assert_eq!(
        ed.visual_block(),
        Some((0, 2, 4, Some(6))),
        "the rectangle is up"
    );
    feed(&mut ed, "d");
    assert_eq!(ed.buffer.text(), "one \nsix r\nten \n", "the block went");
    assert_eq!(
        ed.register.text(),
        "two\nfou\nsix",
        "and is in the register"
    );
    assert_eq!(ed.mode, Mode::Normal);
    assert_eq!(ed.cursor.row, 0, "the cursor is at the block's top left");
    assert_eq!(
        ed.cursor.col, 3,
        "and on the last character of what is left of that row"
    );
    feed(&mut ed, "u");
    assert_eq!(
        ed.buffer.text(),
        source,
        "one `u` takes the whole block back"
    );
    // `y`: nothing changes, and the register has the rows.
    ed.buffer = Buffer::from_text(source);
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "<C-v>");
    ed.cursor.row = 1;
    ed.cursor.col = 6;
    feed(&mut ed, "y");
    assert_eq!(ed.buffer.text(), source);
    assert_eq!(ed.register.text(), "two\nfou");
    // `c`: the block goes, and the typing goes where its top left was. It
    // types on the *first* row: typing on every row is `I`, whose typing is
    // one `u` because it deletes nothing.
    ed.buffer = Buffer::from_text(source);
    ed.cursor.row = 0;
    ed.cursor.col = 0;
    feed(&mut ed, "<C-v>");
    ed.cursor.row = 1;
    ed.cursor.col = 2;
    feed(&mut ed, "c");
    assert_eq!(ed.mode, Mode::Insert, "and typing follows");
    feed(&mut ed, "x<Esc>");
    assert_eq!(
        ed.buffer.text(),
        "x two\nx four\nten six\n",
        "the block went, and the typing went on every row of it"
    );
    feed(&mut ed, "u");
    assert_eq!(
        ed.buffer.text(),
        source,
        "and one `u` takes the whole change"
    );
}

#[test]
fn a_block_can_be_selected_by_keys_through_normal() {
    // The smoke path: `:normal` with the keys in it, which is how a session
    // can be driven from the outside. The cursor moves to the block's left
    // edge *before* `<C-v>`, since that is where the rectangle starts.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\nsix four\nten six\nab\n");
    // One `:normal`, since each one puts the cursor at the start of its
    // line: the keys are a sequence, and a sequence is one call.
    // Everything in *one* `:normal`: each one puts the cursor at the start
    // of its line before running its keys, so splitting the sequence would
    // move the block's other end.
    ed.execute_cmdline("normal llll<C-v>jjlld");
    assert_eq!(
        ed.buffer.text(),
        "one \nsix r\nten \nab\n",
        "the rectangle's columns went from every row"
    );
    assert_eq!(ed.register.text(), "two\nfou\nsix");
    ed.execute_cmdline("normal u");
    assert_eq!(
        ed.buffer.text(),
        "one two\nsix four\nten six\nab\n",
        "and one undo takes it all back"
    );
}

#[test]
fn a_block_goes_back_in_columns() {
    // Rows of the same width, so the columns a person sees are the columns
    // the test is about. Each case starts from the same editor, since a put
    // that has already happened is a different document.
    const SOURCE: &str = "one two\ntwo six\nsix ten\nab\n";
    let yanked = || {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(SOURCE);
        ed.cursor.row = 0;
        ed.cursor.col = 4;
        feed(&mut ed, "<C-v>");
        ed.cursor.row = 2;
        ed.cursor.col = 6;
        feed(&mut ed, "y");
        assert!(
            ed.register.is_blockwise(),
            "the register remembers the shape"
        );
        assert_eq!(ed.register.text(), "two\nsix\nten");
        ed
    };

    // `p` puts it after the cursor's column, on the rows below it.
    let mut ed = yanked();
    ed.cursor.row = 0;
    ed.cursor.col = 6;
    feed(&mut ed, "p");
    assert_eq!(
        ed.buffer.text(),
        "one twotwo\ntwo sixsix\nsix tenten\nab\n",
        "the block's three rows, on the cursor's row and the two below it"
    );
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), SOURCE, "one `u` takes the whole put back");

    // `P` puts it *at* the cursor's column rather than after it.
    let mut ed = yanked();
    ed.cursor.row = 1;
    ed.cursor.col = 0;
    feed(&mut ed, "P");
    assert_eq!(
        ed.buffer.text(),
        "one two\ntwotwo six\nsixsix ten\ntenab\n",
        "at column zero, on the cursor's row and the two below it"
    );

    // A row that is too short for the column is padded with spaces, which is
    // the one place this editor invents text: a column is a place, and a
    // place past the end of a line is a space. The block reaches the short
    // row by covering it, since its rows are consecutive.
    let mut ed = yanked();
    ed.cursor.row = 2;
    ed.cursor.col = 5;
    feed(&mut ed, "P");
    assert_eq!(
        ed.buffer.text(),
        "one two\ntwo six\nsix ttwoen\nab   six\n",
        "`two` into the row at the cursor, `six` padded into the short one"
    );

    // A count repeats the rectangle sideways: the columns go in, then the
    // same columns again after them.
    let mut ed = yanked();
    ed.cursor.row = 0;
    ed.cursor.col = 0;
    feed(&mut ed, "2P");
    assert_eq!(
        ed.buffer.text(),
        "twotwoone two\nsixsixtwo six\ntentensix ten\nab\n",
        "two copies side by side"
    );
}

#[test]
fn typing_on_a_block_types_on_every_row_of_it() {
    const SOURCE: &str = "one two\ntwo six\nsix ten\nab\n";
    let rectangle = |first: usize, last: usize, left: usize, right: usize| {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(SOURCE);
        ed.selection = Some(Selection {
            anchor: Cursor::new(first, left),
            kind: VisualKind::Blockwise,
            to_end: false,
        });
        ed.cursor = Cursor::new(last, right);
        ed.mode = Mode::Visual;
        ed
    };

    // `I` types at the block's left column, on every row of it.
    let mut ed = rectangle(0, 2, 4, 6);
    feed(&mut ed, "I");
    assert_eq!(ed.mode, Mode::Insert);
    feed(&mut ed, "X<Esc>");
    assert_eq!(
        ed.buffer.text(),
        "one Xtwo\ntwo Xsix\nsix Xten\nab\n",
        "one keystroke, three rows"
    );
    feed(&mut ed, "u");
    assert_eq!(
        ed.buffer.text(),
        SOURCE,
        "and one `u` takes the session back"
    );

    // `A` types one past the block's right column, which is what appending
    // to a rectangle means.
    let mut ed = rectangle(0, 2, 4, 6);
    feed(&mut ed, "A");
    feed(&mut ed, "!<Esc>");
    assert_eq!(
        ed.buffer.text(),
        "one two!\ntwo six!\nsix ten!\nab\n",
        "at the end of the block, not of the line"
    );

    // A row that does not reach the block is not part of it: `v_b_I` says a
    // line that is too short "remains unmodified", and that is the difference
    // between typing at the *start* of a block and appending to it.
    let mut ed = rectangle(1, 3, 4, 6);
    feed(&mut ed, "I");
    feed(&mut ed, "X<Esc>");
    assert_eq!(
        ed.buffer.text(),
        "one two\ntwo Xsix\nsix Xten\nab\n",
        "`ab` never reached the block, so nothing was typed on it"
    );
    // `A` appends instead, and pads a short row out to the block's edge
    // (`v_b_A`, case 2), so the text lines up in a column.
    let mut ed = rectangle(1, 3, 4, 6);
    feed(&mut ed, "A");
    feed(&mut ed, "X<Esc>");
    assert_eq!(
        ed.buffer.text(),
        "one two\ntwo sixX\nsix tenX\nab     X\n",
        "appended one past the block's edge, and `ab` padded out to it"
    );

    // Backspace replays too, which is what makes a mistake in a block
    // insert undoable a keystroke at a time.
    let mut ed = rectangle(0, 2, 0, 2);
    feed(&mut ed, "I");
    feed(&mut ed, "Q<BS>Z<Esc>");
    assert_eq!(
        ed.buffer.text(),
        "Zone two\nZtwo six\nZsix ten\nab\n",
        "the backspace went from every row as well"
    );

    // `c` takes the block out and types on every row in its place, and the
    // whole thing — deletion and typing — is one `u`: the command opens a
    // group and the insert session's nests inside it.
    let mut ed = rectangle(0, 2, 4, 6);
    feed(&mut ed, "c");
    assert_eq!(ed.mode, Mode::Insert);
    feed(&mut ed, "yes<Esc>");
    assert_eq!(
        ed.buffer.text(),
        "one yes\ntwo yes\nsix yes\nab\n",
        "the block was deleted and the typing went where it was"
    );
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), SOURCE, "one `u`, the whole change");
}

#[test]
fn a_block_can_be_ragged_to_the_end_of_every_line() {
    const SOURCE: &str = "one two\ntwo six\nsix ten\nab\n";
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(SOURCE);
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "<C-v>");
    ed.cursor.row = 2;
    ed.cursor.col = 4;
    // `$` makes the right edge the end of each line, and takes the cursor
    // there — the block is now ragged, not a rectangle.
    feed(&mut ed, "$");
    assert_eq!(
        ed.visual_block(),
        Some((0, 2, 4, None)),
        "the right edge is the end of the line"
    );
    assert_eq!(ed.cursor.col, 6, "and the cursor is at the end of its line");
    assert_eq!(
        ed.block_spans()
            .iter()
            .map(|(start, end)| ed.buffer.slice_text(*start, *end))
            .collect::<Vec<_>>(),
        vec!["two", "six", "ten"],
        "each row takes what is left of it"
    );
    // The short row is inside the block now, and takes what it has.
    ed.cursor.row = 3;
    assert_eq!(ed.visual_block(), Some((0, 3, 4, None)));
    assert_eq!(
        ed.block_spans()
            .iter()
            .map(|(start, end)| ed.buffer.slice_text(*start, *end))
            .collect::<Vec<_>>(),
        vec!["two", "six", "ten", ""],
        "`ab` has nothing past column four"
    );
    // `d` takes the tails out, and one `u` puts them back.
    feed(&mut ed, "d");
    assert_eq!(ed.buffer.text(), "one \ntwo \nsix \nab\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), SOURCE);
    // `A` on a ragged block types at the end of every line.
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "<C-v>");
    ed.cursor.row = 2;
    feed(&mut ed, "$");
    feed(&mut ed, "A");
    feed(&mut ed, "!<Esc>");
    assert_eq!(
        ed.buffer.text(),
        "one two!\ntwo six!\nsix ten!\nab\n",
        "at the end of each line, not at a column"
    );
    // `$` with any other selection is the line-end motion it always was.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(SOURCE);
    ed.cursor.row = 0;
    ed.cursor.col = 0;
    feed(&mut ed, "v$");
    assert_eq!(ed.cursor.col, 6);
    assert!(ed.visual_span().is_some(), "the selection grew");
    assert!(
        ed.selection.is_some_and(|selection| !selection.to_end),
        "and it is not a block's edge"
    );
}

#[test]
fn an_operator_can_be_forced_to_work_blockwise() {
    // Vim's `o_CTRL-V`: `d<C-V>j` "deletes the character under the cursor and
    // the character below the cursor" — the corners of the block are where the
    // cursor was and where the motion took it.
    const SOURCE: &str = "one two\ntwo six\nsix ten\n";
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(SOURCE);
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "d<C-v>j");
    assert_eq!(
        ed.buffer.text(),
        "one wo\ntwo ix\nsix ten\n",
        "one column, two rows"
    );
    assert_eq!(
        ed.register.text(),
        "t\ns",
        "and the block is in the register"
    );
    assert_eq!(ed.mode, Mode::Normal);
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), SOURCE, "one `u`");

    // The motion is *one* movement command, counted if it needs to be: `2l`
    // moves two columns right, and the block is three columns of one row.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(SOURCE);
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "d<C-v>2l");
    assert_eq!(ed.buffer.text(), "one \ntwo six\nsix ten\n");
    assert_eq!(ed.register.text(), "two");

    // `y` and `c` are the same operator with a different verb, and the cursor
    // goes back to the block's corner.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(SOURCE);
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "y<C-v>j");
    assert_eq!(ed.buffer.text(), SOURCE, "a yank leaves the text alone");
    assert_eq!(ed.register.text(), "t\ns");
    assert_eq!((ed.cursor.row, ed.cursor.col), (0, 4), "at the corner");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(SOURCE);
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "c<C-v>j");
    assert_eq!(ed.mode, Mode::Insert, "and `c` types where the block was");
    assert_eq!(ed.buffer.text(), "one wo\ntwo ix\nsix ten\n");

    // Without an operator, `<C-v>` is the selection it always was.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text(SOURCE);
    feed(&mut ed, "<C-v>");
    assert_eq!(
        ed.selection.map(|selection| selection.kind),
        Some(VisualKind::Blockwise),
        "a selection, not a forced motion"
    );
    assert_eq!(ed.forced_shape, None, "nothing was forced");
}

#[test]
fn a_block_may_reach_past_the_end_of_a_short_line() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\ntwo\nsix ten\n");
    ed.cursor.row = 0;
    ed.cursor.col = 4;
    feed(&mut ed, "<C-v>");
    // Down to the short line, then three columns right: `two` is three characters
    // long, so the cursor ends past its end — which is what a rectangle reaching
    // into a shorter line needs, and what `'virtualedit'` is for.
    feed(&mut ed, "j");
    assert_eq!(ed.cursor.row, 1);
    assert_eq!(
        ed.cursor.col, 3,
        "coming down onto a short line stops at its end"
    );
    feed(&mut ed, "lll");
    assert_eq!(ed.cursor.col, 6, "and then a block may go past it");
    assert_eq!(ed.visual_block(), Some((0, 1, 4, Some(6))));
    assert_eq!(
        ed.block_spans()
            .iter()
            .map(|(start, end)| ed.buffer.slice_text(*start, *end))
            .collect::<Vec<_>>(),
        vec!["two", ""],
        "the short line takes what it has, which is nothing there"
    );
    // The rectangle is real: deleting it takes the columns from each row.
    feed(&mut ed, "d");
    assert_eq!(ed.buffer.text(), "one \ntwo\nsix ten\n");
    // Normal mode still stops at the last character: only a block is free.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    ed.cursor.row = 0;
    feed(&mut ed, "lllll");
    assert_eq!(ed.cursor.col, 2, "`l` stops at the last character");
    feed(&mut ed, "<C-v>");
    feed(&mut ed, "llll");
    assert_eq!(ed.cursor.col, 6, "but a block may go past it");
}

#[test]
fn a_characterwise_selection_deletes_what_it_shows() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abcdef\n");
    feed(&mut ed, "vlld");
    assert_eq!(ed.buffer.text(), "def\n", "the cursor's character included");
    assert_eq!(ed.mode, Mode::Normal);
    assert_eq!(ed.selection, None);
    assert_eq!(ed.register.text(), "abc");
}

#[test]
fn a_selection_made_backwards_covers_the_same_ground() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abcdef\n");
    ed.cursor = Cursor::new(0, 2);
    feed(&mut ed, "vhhd");
    assert_eq!(ed.buffer.text(), "def\n");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
}

#[test]
fn a_linewise_selection_takes_whole_lines() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    feed(&mut ed, "Vjd");
    assert_eq!(
        ed.buffer.text(),
        "three\n",
        "two lines, terminators and all"
    );
    assert_eq!(ed.register.text(), "one\ntwo\n");
    assert!(ed.register.is_linewise());

    // A linewise selection on the last line, and one line's worth of it.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "GVd");
    assert_eq!(ed.buffer.text(), "one\n");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "Vd");
    assert_eq!(ed.buffer.text(), "two\n", "`V` then `d` is `dd`");
}

#[test]
fn visual_mode_shares_the_normal_modes_motions() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two three\n");
    // `w` is the same motion, but a visual selection includes the character
    // the cursor lands on — vim's rule, and the reason `vwd` is not `dw`.
    feed(&mut ed, "vwd");
    assert_eq!(ed.buffer.text(), "wo three\n");

    // Multi-key motions too, through the same fallback: a backwards
    // selection runs from the cursor back through the anchor's character.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    ed.cursor = Cursor::new(2, 0);
    feed(&mut ed, "vggd");
    assert_eq!(ed.buffer.text(), "hree\n");
}

#[test]
fn escape_leaves_visual_mode_without_editing() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abcdef\n");
    feed(&mut ed, "vll<Esc>");
    assert_eq!(ed.mode, Mode::Normal);
    assert_eq!(ed.selection, None);
    assert_eq!(ed.buffer.text(), "abcdef\n");
    assert_eq!(ed.cursor, Cursor::new(0, 2));
}

#[test]
fn v_and_v_toggle_the_selection_and_its_kind() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "v");
    assert_eq!(ed.mode, Mode::Visual);
    feed(&mut ed, "v");
    assert_eq!(ed.mode, Mode::Normal, "the same key again leaves");

    feed(&mut ed, "vV");
    assert_eq!(ed.mode, Mode::Visual);
    assert_eq!(
        ed.selection.map(|s| s.kind),
        Some(VisualKind::Linewise),
        "`V` changes what the selection covers"
    );
    feed(&mut ed, "v");
    assert_eq!(ed.selection.map(|s| s.kind), Some(VisualKind::Charwise));
}

#[test]
fn o_puts_the_cursor_on_the_other_end() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abcdef\n");
    feed(&mut ed, "vllo");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "the cursor is on the anchor");
    assert_eq!(ed.mode, Mode::Visual);
    assert_eq!(
        ed.visual_span(),
        Some((0, 3)),
        "and the selection itself is unchanged"
    );
    feed(&mut ed, "d");
    assert_eq!(ed.buffer.text(), "def\n");
}

#[test]
fn a_count_extends_the_selection() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abcdef\n");
    feed(&mut ed, "v3ld");
    assert_eq!(
        ed.buffer.text(),
        "ef\n",
        "four characters, the cursor's too"
    );
}

#[test]
fn visual_change_and_yank_act_on_the_selection() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abcdef\n");
    feed(&mut ed, "vlc");
    assert_eq!(ed.mode, Mode::Insert);
    assert_eq!(ed.buffer.text(), "cdef\n");
    feed(&mut ed, "X<Esc>");
    assert_eq!(ed.buffer.text(), "Xcdef\n");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("abcdef\n");
    feed(&mut ed, "vly");
    assert_eq!(ed.mode, Mode::Normal, "yanking leaves visual mode");
    assert_eq!(ed.buffer.text(), "abcdef\n");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "at the start of the yank");
    feed(&mut ed, "p");
    assert_eq!(ed.buffer.text(), "aabbcdef\n");
}

#[test]
fn a_visual_edit_is_one_step() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    feed(&mut ed, "Vjd");
    assert_eq!(ed.buffer.text(), "three\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "one\ntwo\nthree\n");
}

#[test]
fn visual_put_replaces_the_selection() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    // Yank a word, then select another one and put over it. The replaced
    // text is *not* what lands in the register: it stays as it was.
    feed(&mut ed, "yw");
    assert_eq!(ed.register.text(), "one ");
    feed(&mut ed, "vwp");
    assert_eq!(
        ed.buffer.text(),
        "one wo\n",
        "the selection ran through the character `w` landed on"
    );
    assert_eq!(ed.register.text(), "one ", "and the register is untouched");
    assert_eq!(ed.mode, Mode::Normal);
}

#[test]
fn the_status_line_says_what_is_selected() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "v");
    assert!(ed.status().contains("VISUAL"), "{}", ed.status());
    assert!(!ed.status().contains("LINE"), "{}", ed.status());
    feed(&mut ed, "V");
    assert!(ed.status().contains("VISUAL LINE"), "{}", ed.status());
    feed(&mut ed, "<Esc>");
    assert!(ed.status().contains("NORMAL"), "{}", ed.status());
}

#[test]
fn a_triple_click_selects_the_line_and_a_quadruple_a_block() {
    use inkstone_rpc::{PointerKind, PointerMsg};
    let mut ed = Editor::for_tests();
    ed.viewport.width = 40;
    ed.viewport.height = 12;
    ed.buffer = Buffer::from_text("one two\nthree four\n");
    // Triple: the line, terminator and all — "triple → line".
    ed.handle_pointer(&PointerMsg::click(PointerKind::Press, 5, 1, 3));
    assert_eq!(ed.selection.map(|s| s.kind), Some(VisualKind::Linewise));
    let (start, end) = ed.visual_span().expect("a selection");
    assert_eq!(ed.buffer.slice_text(start, end), "three four\n");
    // Quadruple: a rectangle, which is the whole line until a drag gives it
    // a width — "quadruple → rectangular block".
    ed.handle_pointer(&PointerMsg::click(PointerKind::Press, 5, 1, 4));
    assert_eq!(ed.selection.map(|s| s.kind), Some(VisualKind::Blockwise));
    assert_eq!(
        ed.visual_block()
            .map(|(first, last, left, _)| (first, last, left)),
        Some((1, 1, 0)),
        "the whole line, from its first column"
    );
}
