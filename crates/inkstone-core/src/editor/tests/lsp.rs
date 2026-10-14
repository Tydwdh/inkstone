use super::*;

#[test]
fn a_snippet_is_typed_and_stepped_through() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "i");
    ed.type_text("fn");
    let item = CompletionItem {
        label: "fn".to_string(),
        insert: "fn ${1:name}() {\n    $0\n}".to_string(),
        detail: String::new(),
        snippet: true,
    };
    ed.set_completions(vec![item.clone()]);
    // What is typed is the snippet's text, with the placeholders' defaults
    // in it and the markers gone.
    assert_eq!(ed.buffer.line_string(0), "fn name() {");
    assert_eq!(ed.buffer.line_string(1), "    ");
    let stops: Vec<(usize, usize)> = ed.snippet_stops.iter().map(|stop| stop.span).collect();
    assert_eq!(stops.len(), 2, "the name and the end");
    // `<Tab>` goes to the first stop — its text selected, so the cursor is
    // at the end of it — and again to the end of the snippet.
    feed(&mut ed, "<Tab>");
    assert_eq!(ed.cursor.char_idx(&ed.buffer), stops[0].1 - 1);
    assert!(ed.selection.is_some(), "and its text is selected");
    feed(&mut ed, "<Tab>");
    assert_eq!(ed.cursor.char_idx(&ed.buffer), stops[1].0);
    assert!(ed.selection.is_none(), "a place with nothing in it");
    // Past the last one there is nothing to step to, so the key is a tab.
    let before = ed.buffer.len_chars();
    feed(&mut ed, "<Tab>");
    assert_eq!(ed.buffer.len_chars(), before + 1, "a tab, as it always was");
    assert!(ed.snippet_stops.is_empty(), "and the session is over");
    // `<S-Tab>` walks back, and a plain tab is a tab when nothing is up.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "i");
    ed.set_completions(vec![item]);
    feed(&mut ed, "<Tab>");
    let first = ed.cursor.char_idx(&ed.buffer);
    feed(&mut ed, "<Tab>");
    assert_ne!(ed.cursor.char_idx(&ed.buffer), first);
    feed(&mut ed, "<S-Tab>");
    assert_eq!(
        ed.cursor.char_idx(&ed.buffer),
        first,
        "back to the first stop"
    );
}

#[test]
fn what_the_server_said_can_be_shown_in_a_window() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    // Nothing said, nothing to show.
    ed.execute_cmdline("lopen");
    assert_eq!(ed.message, "No diagnostics");
    assert_eq!(ed.windows.len(), 1);

    ed.set_diagnostics(vec![
        Diagnostic {
            start: (1, 2),
            end: (1, 5),
            severity: crate::diagnostic::Severity::Warning,
            message: "something about two".to_string(),
        },
        Diagnostic {
            start: (2, 0),
            end: (2, 1),
            severity: crate::diagnostic::Severity::Error,
            message: "something about three".to_string(),
        },
    ]);
    ed.execute_cmdline("lopen");
    assert_eq!(ed.windows.len(), 2);
    assert_eq!(ed.list, Some(List::Diagnostics));
    assert_eq!(ed.message, "2 diagnostics in 1 file");
    let text = ed.buffer.text();
    assert!(text.starts_with("1 [No Name]:2:3 "), "{text:?}");
    assert!(text.contains("something about two"), "{text:?}");
    // A list is not for editing, and `<CR>` goes to the diagnostic's place.
    feed(&mut ed, "x");
    assert!(ed.message.starts_with("E21"), "{}", ed.message);
    feed(&mut ed, "j");
    feed(&mut ed, "<CR>");
    assert_eq!(ed.windows.len(), 1, "the window went away");
    assert_eq!(ed.list, None);
    assert!(
        matches!((ed.cursor.row, ed.cursor.col), (1, 2) | (2, 0)),
        "on one of the diagnostics: {:?}",
        (ed.cursor.row, ed.cursor.col)
    );
    // A second document's diagnostics are in the list too, with their file.
    let other = std::env::temp_dir().join("inkstone-diags").join("other.rs");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    std::fs::write(&other, "fn main() {}\n").unwrap();
    ed.open_file(&other).unwrap();
    ed.set_diagnostics(vec![Diagnostic {
        start: (0, 0),
        end: (0, 3),
        severity: crate::diagnostic::Severity::Error,
        message: "something about the other file".to_string(),
    }]);
    // Back to the first document, where the window is opened from.
    ed.show_document(0);
    ed.execute_cmdline("lopen");
    let text = ed.buffer.text();
    assert!(
        text.contains("other.rs:1:1"),
        "both files are in the list: {text:?}"
    );
    assert!(
        text.contains("[No Name]:2:3"),
        "and so is the one on screen: {text:?}"
    );
    // `<CR>` on the line that names the other file goes there.
    let line = text
        .lines()
        .position(|line| line.contains("other.rs"))
        .expect("a line for the other file");
    ed.cursor.row = line;
    ed.open_place_at_cursor();
    assert!(
        ed.buffer
            .path()
            .is_some_and(|path| path.ends_with("other.rs")),
        "went to the file the line named: {:?}",
        ed.buffer.path()
    );
    assert_eq!((ed.cursor.row, ed.cursor.col), (0, 0));

    // And `:lclose` closes it without going anywhere. (The list now holds
    // every open document's diagnostics, so where the walk is depends on the
    // order the documents are in; what matters is that closing moves nothing.)
    ed.execute_cmdline("lopen");
    let before = (ed.cursor.row, ed.cursor.col);
    ed.execute_cmdline("lclose");
    assert_eq!(ed.windows.len(), 1);
    assert_eq!((ed.cursor.row, ed.cursor.col), before);
}

#[test]
fn a_range_asks_the_server_about_that_range() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
    // With a range, the ask carries it; without one it does not.
    ed.execute_cmdline("2,3format");
    match ed.take_lsp_ask() {
        Some(Ask::Format { from, to, .. }) => {
            assert_eq!((from, to), (Some(1), Some(2)), "the lines the range named");
        }
        other => panic!("expected a format ask, got {other:?}"),
    }
    ed.execute_cmdline("format");
    match ed.take_lsp_ask() {
        Some(Ask::Format { from, to, .. }) => assert_eq!((from, to), (None, None)),
        other => panic!("expected a format ask, got {other:?}"),
    }
}

#[test]
fn format_on_save_asks_after_the_write_and_only_for_a_plain_one() {
    let dir = std::env::temp_dir().join("inkstone-fmt-save");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("one.txt");
    std::fs::write(&one, "a\n").unwrap();
    let other = dir.join("other.txt");

    let mut ed = Editor::for_tests();
    ed.open_file(&one).unwrap();

    // Off by default: saving says nothing to a server.
    ed.type_text("more\n");
    ed.execute_cmdline("w");
    assert!(ed.lsp_ask.is_none(), "no server is asked anything");
    assert!(ed.message.contains("written"), "{}", ed.message);

    // On: the write happens, and then the layout is asked for — *after* it,
    // since a server that never answers must not cost a person their save.
    ed.options.borrow_mut().formatonsave = true;
    ed.type_text("more\n");
    ed.execute_cmdline("w");
    assert_eq!(
        std::fs::read_to_string(&one).unwrap(),
        "more\nmore\na\n",
        "the file was written before anything was asked"
    );
    match ed.lsp_ask {
        Some(Ask::Format { save, from, to, .. }) => {
            assert!(save, "the ask says it is part of a save");
            assert_eq!((from, to), (None, None), "and that it is the whole file");
        }
        ref other => panic!("expected a format ask, got {other:?}"),
    }

    // Writing to *another* file is not saving this one: no layout is asked
    // for, since what would be laid out is not what was written.
    ed.lsp_ask = None;
    ed.type_text("more\n");
    ed.execute_cmdline(&format!("w {}", other.display()));
    assert!(
        ed.lsp_ask.is_none(),
        "a write elsewhere does not format this file"
    );
    assert_eq!(
        std::fs::read_to_string(&other).unwrap(),
        "more\nmore\nmore\na\n"
    );
}

#[test]
fn completion_types_the_item_it_picks() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("counter counter_one\ncolour\ncount\n");
    feed(&mut ed, "Gocou");
    feed(&mut ed, "<C-n>");
    // `<C-n>` types the word it picks: the list is something to type from,
    // not a menu to confirm.
    assert_eq!(
        ed.buffer.text(),
        "counter counter_one\ncolour\ncount\ncount\n"
    );
    assert_eq!(
        ed.cursor.col, 5,
        "the cursor is at the end of what it typed"
    );
    let completion = ed.completion.as_ref().expect("the list is up");
    let labels: Vec<&str> = completion.shown.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["count", "counter", "counter_one"], "{labels:?}");
    assert_eq!(completion.typed, "cou", "what was typed before it opened");
    assert_eq!(
        ed.buffer.char_to_line_col(completion.start),
        (3, 0),
        "where the word being completed starts"
    );

    // `<C-n>` again moves on and types that; `<C-p>` comes back, and wraps.
    feed(&mut ed, "<C-n>");
    assert_eq!(
        ed.buffer.text(),
        "counter counter_one\ncolour\ncount\ncounter\n"
    );
    feed(&mut ed, "<C-n>");
    assert_eq!(
        ed.buffer.text(),
        "counter counter_one\ncolour\ncount\ncounter_one\n"
    );
    feed(&mut ed, "<C-n>");
    assert_eq!(
        ed.buffer.text(),
        "counter counter_one\ncolour\ncount\ncount\n",
        "wrapping"
    );
    feed(&mut ed, "<C-p>");
    assert_eq!(
        ed.buffer.text(),
        "counter counter_one\ncolour\ncount\ncounter_one\n",
        "and back the other way"
    );

    // Typing narrows the list, by what is in front of the cursor:
    // backspacing to `counte` leaves everything that starts with it.
    feed(&mut ed, "<BS><BS><BS><BS><BS><BS>");
    assert_eq!(
        ed.buffer.text(),
        "counter counter_one\ncolour\ncount\ncount\n",
        "back to the letters that were typed"
    );
    let labels: Vec<String> = ed
        .completion
        .as_ref()
        .expect("`count` still fits three")
        .shown
        .iter()
        .map(|item| item.label.clone())
        .collect();
    assert_eq!(
        labels,
        ["count", "counter", "counter_one"],
        "`count` fits them all"
    );
    feed(&mut ed, "er_");
    let labels: Vec<String> = ed
        .completion
        .as_ref()
        .expect("`counter_` fits one")
        .shown
        .iter()
        .map(|item| item.label.clone())
        .collect();
    assert_eq!(labels, ["counter_one"]);
    feed(&mut ed, "z");
    assert!(
        ed.completion.is_none(),
        "a word nothing fits closes the list"
    );

    // `<C-y>` keeps what is typed and closes; `<C-e>` puts back what was
    // there before the list opened.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("counter\ncount\n");
    feed(&mut ed, "Gocou<C-n>");
    assert_eq!(ed.buffer.text(), "counter\ncount\ncount\n");
    feed(&mut ed, "<C-y>");
    assert!(ed.completion.is_none());
    assert_eq!(ed.buffer.text(), "counter\ncount\ncount\n", "kept");
    assert_eq!(ed.mode, Mode::Insert, "and it stays in insert mode");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("counter\ncount\n");
    feed(&mut ed, "Gocou<C-n>");
    feed(&mut ed, "<C-e>");
    assert!(ed.completion.is_none());
    assert_eq!(ed.buffer.text(), "counter\ncount\ncou\n", "put back");

    // And leaving insert mode keeps what it typed, which is what `<Esc>`
    // means when a list is up.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("counter\ncount\n");
    feed(&mut ed, "Gocou<C-n><Esc>");
    assert!(ed.completion.is_none());
    assert_eq!(ed.buffer.text(), "counter\ncount\ncount\n");

    // With nothing to offer, the list never opens.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("alpha\n");
    feed(&mut ed, "Gozz<C-n>");
    assert!(ed.completion.is_none());
    assert_eq!(ed.buffer.text(), "alpha\nzz\n");
}

#[test]
fn the_completion_popup_is_drawn_under_the_cursor() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("co\ncounter\ncount\ncolour\n");
    ed.viewport.width = 24;
    ed.viewport.height = 8;
    // Insert at the end of the first line and ask for what could follow.
    feed(&mut ed, "A");
    feed(&mut ed, "<C-n>");
    assert_eq!(
        ed.completion.as_ref().unwrap().shown.len(),
        3,
        "three words"
    );

    // The list is drawn over the lines below, starting at the column the
    // word being completed starts at — which is where the cursor is.
    let composed = ed.compose();
    let row = |y: u16| composed.grid.row_text(y).trim_end().to_string();
    assert_eq!(
        row(0),
        "counter",
        "the line being typed, with the pick in it"
    );
    // The popup writes its item and then blanks the rest of its own width,
    // so what is under it shows past its edge.
    assert!(row(2).starts_with("count"), "{}", row(2));
    assert!(row(3).starts_with("colour"), "{}", row(3));

    // The pick is drawn in reverse video, the others are not.
    let picked = composed.grid.get(0, 1).clone();
    let other = composed.grid.get(0, 2).clone();
    assert_ne!(picked.attrs & inkstone_rpc::ATTR_REVERSE, 0, "{}", row(1));
    assert_eq!(other.attrs & inkstone_rpc::ATTR_REVERSE, 0, "{}", row(2));
}

#[test]
fn diagnostics_are_drawn_where_the_server_put_them() {
    use crate::diagnostic::{Diagnostic, Severity};
    let dir = std::env::temp_dir().join("inkstone-editor-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let first = dir.join("diagnostics-first.txt");
    std::fs::write(&first, "alpha bravo\ncharlie\ndelta\n").unwrap();
    let mut ed = Editor::for_tests();
    ed.load_file(&first).unwrap();
    ed.viewport.width = 30;
    ed.viewport.height = 8;
    // The severity's colour is the line number's, so the gutter has to be
    // there: the underline is on the text either way.
    ed.options.borrow_mut().number = true;
    ed.set_diagnostics(vec![
        Diagnostic::new((0, 6), (0, 11), Severity::Error, "unknown word"),
        Diagnostic::new((2, 0), (2, 5), Severity::Warning, "shadowed"),
    ]);
    assert_eq!(ed.diagnostics.len(), 2);

    // The range is underlined, the rest of the line is not, and the line
    // number carries the severity — the one part of a row that is ours.
    let composed = ed.compose();
    let cell = |x: u16, y: u16| composed.grid.get(x, y).clone();
    let underline = inkstone_rpc::ATTR_UNDERLINE;
    // The text of a line starts after its line number.
    let gutter = crate::render::gutter_width(ed.buffer.line_count(), true);
    let at = |col: u16| gutter + col;
    assert_eq!(
        cell(at(6), 0).attrs & underline,
        underline,
        "the first character"
    );
    assert_eq!(
        cell(at(10), 0).attrs & underline,
        underline,
        "the last is included"
    );
    assert_eq!(
        cell(at(5), 0).attrs & underline,
        0,
        "the space before it is not"
    );
    assert_eq!(cell(at(11), 0).attrs & underline, 0, "nor the one after");
    let error = Severity::Error.color();
    let warning = Severity::Warning.color();
    assert_eq!(
        cell(0, 0).fg,
        error,
        "the gutter of the line with the error"
    );
    assert_eq!(cell(0, 2).fg, warning, "and of the one with the warning");
    assert_ne!(cell(0, 1).fg, error, "a line with neither is plain");

    // `]d` and `[d` walk them, wrapping, and say what they are.
    ed.cursor.row = 1;
    feed(&mut ed, "]d");
    assert_eq!(ed.cursor.row, 2, "the next one after this line");
    assert_eq!(ed.message, "warning: shadowed");
    feed(&mut ed, "]d");
    assert_eq!(ed.cursor.row, 0, "and around");
    assert_eq!(ed.message, "error: unknown word");
    feed(&mut ed, "[d");
    assert_eq!(ed.cursor.row, 2, "back the other way");

    // `:diagnostics` lists them.
    ed.execute_cmdline("diagnostics");
    assert!(ed.message.starts_with("2 diagnostics"), "{}", ed.message);
    assert!(
        ed.message.contains("1:7 error: unknown word"),
        "{}",
        ed.message
    );

    // Every document keeps its own.
    let other = dir.join("diagnostics-other.txt");
    std::fs::write(&other, "nothing wrong\n").unwrap();
    ed.execute_cmdline(&format!("e {}", other.display()));
    assert!(
        ed.diagnostics.is_empty(),
        "a file of its own, and its own list"
    );
    ed.execute_cmdline("bp");
    assert_eq!(ed.diagnostics.len(), 2, "and the first one still has them");

    // A document with nothing said about it says so.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("fine\n");
    feed(&mut ed, "]d");
    assert_eq!(ed.message, "No diagnostics");
}

#[test]
fn a_spec_says_what_a_server_said() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("alpha bravo\ncharlie\ndelta\n");
    // One-based lines and columns, the way a compiler writes them.
    ed.execute_cmdline("setdiag 1:7:1:12:error:unknown word;3:1:3:6:warning:shadowed");
    assert_eq!(ed.diagnostics.len(), 2);
    assert_eq!(ed.diagnostics[0].start, (0, 6), "one-based on the way in");
    assert_eq!(
        ed.diagnostics[0].severity,
        crate::diagnostic::Severity::Error
    );
    assert_eq!(ed.diagnostics[1].message, "shadowed");

    // A message may hold anything, a colon included.
    ed.execute_cmdline("setdiag 2:1:2:2:hint:try: this");
    assert_eq!(ed.diagnostics[0].message, "try: this");

    // Anything that is not a spec is refused, and nothing changes.
    let before = ed.diagnostics.clone();
    for bad in ["wide", "1:2:x:4:error:no", "1::2:3:error:no"] {
        ed.execute_cmdline(&format!("setdiag {bad}"));
        assert!(ed.message.starts_with("E475:"), "{bad}: {}", ed.message);
        assert_eq!(ed.diagnostics, before, "{bad} changed nothing");
    }
    ed.execute_cmdline("setdiag");
    assert_eq!(ed.message, "E471: Argument required");

    // A line that is not in the file is clamped to the last one: a server
    // can be a keystroke behind.
    ed.execute_cmdline("setdiag 99:1:99:2:error:gone");
    assert_eq!(ed.diagnostics[0].start.0, 2);
}
