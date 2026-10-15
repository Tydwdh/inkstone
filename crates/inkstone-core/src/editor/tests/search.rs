use super::*;
use inkstone_rpc::{ATTR_BOLD, ATTR_UNDERLINE};

#[test]
fn percent_finds_the_matching_bracket() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("fn main() {\n    let x = (1 + 2);\n}\n");
    // From the brace that opens the function, to the one that closes it.
    ed.cursor = Cursor::new(0, 10);
    feed(&mut ed, "%");
    assert_eq!(ed.cursor, Cursor::new(2, 0), "the brace that closes it");
    // And back the other way, from the closing one.
    feed(&mut ed, "%");
    assert_eq!(ed.cursor, Cursor::new(0, 10));
    // A pair on one line: the parens of the call.
    ed.cursor = Cursor::new(0, 7);
    feed(&mut ed, "%");
    assert_eq!(ed.cursor, Cursor::new(0, 8));
    // A bracket the cursor is not on: the first one on the line.
    ed.cursor = Cursor::new(1, 4);
    feed(&mut ed, "%");
    assert_eq!(
        ed.cursor,
        Cursor::new(1, 18),
        "the paren pair ahead on this line"
    );
    // Brackets of every kind.
    ed.buffer = Buffer::from_text("a[1] b<c> {d}\n");
    ed.cursor = Cursor::new(0, 1);
    feed(&mut ed, "%");
    assert_eq!(ed.cursor, Cursor::new(0, 3), "a square pair");
    // Nothing that matches: said so, and the cursor stays.
    ed.buffer = Buffer::from_text("(unclosed\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "%");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    assert!(ed.message.contains("no match"), "{}", ed.message);
    // No bracket at all on the line.
    ed.buffer = Buffer::from_text("plain words\n");
    feed(&mut ed, "%");
    assert_eq!(ed.message, "no bracket here");
}

#[test]
fn a_global_line_runs_a_command_on_the_lines_a_pattern_finds() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("keep one\nTODO two\nkeep three\nTODO four\n");
    ed.execute_cmdline("g/TODO/d");
    assert_eq!(
        ed.buffer.text(),
        "keep one\nkeep three\n",
        "both TODO lines"
    );
    assert_eq!(ed.message, "2 lines");

    // `:v` is the same thing for the lines that do *not* match.
    ed.buffer = Buffer::from_text("keep one\nTODO two\nkeep three\n");
    ed.execute_cmdline("v/TODO/d");
    assert_eq!(ed.buffer.text(), "TODO two\n");

    // A substitute on each matching line, which is what `:g` is used for
    // when it is not deleting: the cursor goes to the line, and `:s` works
    // on the cursor's line.
    ed.buffer = Buffer::from_text("one TODO\ntwo todo\nthree TODO\n");
    ed.execute_cmdline("g/TODO/s/TODO/done/");
    assert_eq!(ed.buffer.text(), "one done\ntwo todo\nthree done\n");

    // A range says which lines are searched, not which are changed.
    ed.buffer = Buffer::from_text("a\nb\na\n");
    ed.execute_cmdline("2,3g/^a$/d");
    assert_eq!(ed.buffer.text(), "a\nb\n", "line 3, and not line 1");

    // No command prints what matched: one line of message, capped like the
    // other lists.
    ed.buffer = Buffer::from_text("one\none two\nthree\n");
    ed.execute_cmdline("g/one");
    assert_eq!(
        ed.message, "2 lines   1 one   2 one two",
        "line numbers and text"
    );

    // A pattern that finds nothing says so and changes nothing.
    ed.execute_cmdline("g/nothinghere/d");
    assert_eq!(ed.message, "Pattern not found: nothinghere");
    assert_eq!(ed.buffer.text(), "one\none two\nthree\n");

    // A command that is not a command is reported, and the buffer stands.
    ed.execute_cmdline("g/one/wobble");
    assert!(ed.message.contains("Not a command"), "{}", ed.message);
    assert_eq!(ed.buffer.text(), "one\none two\nthree\n");
}

#[test]
fn repeating_a_substitute_with_and_without_its_flags() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("a a a\nb a a\n");
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline("s/a/x/");
    assert_eq!(ed.buffer.text(), "x a a\nb a a\n");
    // `:&` is the same substitute again: the first one on this line, no flags.
    ed.execute_cmdline("&");
    assert_eq!(ed.buffer.text(), "x x a\nb a a\n");
    // `:&&` keeps the flags of the last substitute, so a `g` stays a `g`.
    ed.cursor = Cursor::new(1, 0);
    ed.execute_cmdline("s/a/y/g");
    assert_eq!(
        ed.buffer.text(),
        "x x a\nb y y\n",
        "every match on that line"
    );
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline("&&");
    assert_eq!(
        ed.buffer.text(),
        "x x y\nb y y\n",
        "the last substitute's flags: the `g` came with it"
    );
    // With no substitute behind it, both say so.
    let mut ed = Editor::for_tests();
    ed.execute_cmdline("&&");
    assert!(ed.message.contains("E33"), "{}", ed.message);
}

#[test]
fn a_pattern_is_written_the_vim_way() {
    let mut ed = Editor::for_tests();
    // A group and an alternation: `\(` `\)` and `\|`.
    ed.buffer = Buffer::from_text("one two\nthree four\n");
    ed.cursor = Cursor::new(0, 0);
    ed.execute_cmdline("s/\\(one\\|two\\)/X/g");
    assert_eq!(ed.buffer.text(), "X X\nthree four\n");
    // Vim's `\+`, and a count.
    ed.execute_cmdline("%s/l\\+o/X/g");
    assert_eq!(ed.buffer.text(), "X X\nthree four\n");
    ed.execute_cmdline("%s/two\\{1}/Y/g");
    assert_eq!(
        ed.buffer.text(),
        "X X\nthree four\n",
        "nothing here is `two`"
    );
    // `\<` and `\>` are word boundaries: the `one` inside `throne` is not
    // one.
    ed.buffer = Buffer::from_text("one throne\n");
    ed.execute_cmdline("%s/\\<one\\>/1/g");
    assert_eq!(ed.buffer.text(), "1 throne\n");
    // `\c` in the pattern asks for case-insensitivity, the way the `i` flag
    // does, and wins over it.
    ed.buffer = Buffer::from_text("ONE one\n");
    ed.execute_cmdline("%s/\\cone/x/g");
    assert_eq!(ed.buffer.text(), "x x\n");
    ed.buffer = Buffer::from_text("ONE one\n");
    ed.execute_cmdline("%s/\\CONE/x/g");
    assert_eq!(
        ed.buffer.text(),
        "x one\n",
        "a `\\C` in the pattern says what it means"
    );
    // A group and a literal bracket, with the group used again on the
    // replacement side.
    ed.buffer = Buffer::from_text("aXb aXb\n");
    ed.execute_cmdline("s/a\\(X\\)b/[\\1]/");
    assert_eq!(ed.buffer.text(), "[X] aXb\n");
    ed.execute_cmdline("%s/\\[\\(X\\)\\]/<\\1>/");
    assert_eq!(ed.buffer.text(), "<X> aXb\n");
    // A search takes the same dialect.
    ed.buffer = Buffer::from_text("alpha beta\ngamma alpha\n");
    ed.cursor = Cursor::new(0, 0);
    ed.run_search("\\<beta\\>", crate::search::Direction::Forward);
    assert_eq!(ed.cursor.row, 0, "the one word `beta`");
    // A pattern can say where its own match begins and ends: `\zs` moves the
    // start along and `\ze` the end back, and what they leave is the match.
    ed.buffer = Buffer::from_text("foobar\n");
    ed.execute_cmdline("s/foo\\zsbar/X/");
    assert_eq!(ed.buffer.text(), "fooX\n", "the `bar` is the match");
    ed.buffer = Buffer::from_text("foobar\n");
    ed.execute_cmdline("s/foo\\zebar/end/");
    assert_eq!(
        ed.buffer.text(),
        "endbar\n",
        "the `bar` is still matched, and still not part of the match"
    );
}

#[test]
fn a_list_can_be_narrowed_to_what_matches() {
    let dir = std::env::temp_dir().join("inkstone-filter");
    std::fs::create_dir_all(&dir).unwrap();
    let one = dir.join("one.txt");
    let two = dir.join("two.txt");
    std::fs::write(&one, "a\nb\n").unwrap();
    std::fs::write(&two, "c\n").unwrap();

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("mine\n");
    ed.set_places(vec![
        (one.clone(), 0, 0),
        (one.clone(), 1, 0),
        (two.clone(), 0, 0),
    ]);
    // A pattern keeps the lines it matches, and the places with them.
    ed.execute_cmdline("copen two");
    assert_eq!(ed.list, Some(List::Places));
    assert_eq!(ed.list_targets.len(), 1, "one line survived");
    assert_eq!(ed.list_targets[0].0, two);
    assert_eq!(ed.message, "1 place");
    // The numbers are the ones in the window, not the ones in the list.
    assert!(ed.buffer.text().starts_with("1 "), "{}", ed.buffer.text());
    let window = ed.current_window;
    ed.close_window(window);
    // A pattern that matches nothing opens no window and says so.
    ed.execute_cmdline("copen nothinghere");
    assert_eq!(ed.windows.len(), 1);
    assert!(ed.message.contains("Pattern not found"), "{}", ed.message);
    // A pattern that is not a pattern says so too.
    ed.execute_cmdline("copen [");
    assert!(ed.message.starts_with("E54"), "{}", ed.message);
    // With no pattern the whole list is shown.
    ed.execute_cmdline("copen");
    assert_eq!(ed.list_targets.len(), 3);
    let window = ed.current_window;
    ed.close_window(window);

    // Diagnostics filter the same way, on the line each one is written as.
    ed.set_diagnostics(vec![
        Diagnostic {
            start: (0, 0),
            end: (0, 1),
            severity: crate::diagnostic::Severity::Error,
            message: "an error about one".to_string(),
        },
        Diagnostic {
            start: (1, 0),
            end: (1, 1),
            severity: crate::diagnostic::Severity::Warning,
            message: "a warning about two".to_string(),
        },
    ]);
    ed.execute_cmdline("lopen warning");
    assert_eq!(ed.list_targets.len(), 1);
    assert_eq!(ed.list_targets[0].1, 1, "the warning's line");
    let window = ed.current_window;
    ed.close_window(window);
    ed.execute_cmdline("lopen error");
    assert_eq!(ed.list_targets.len(), 1);
    assert_eq!(ed.message, "1 diagnostic in 1 file");
}

#[test]
fn a_search_moves_the_cursor_between_matches() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two one\ntwo\n");
    feed(&mut ed, "/two<Enter>");
    assert_eq!(
        ed.cursor,
        Cursor::new(0, 4),
        "the first match after the cursor"
    );
    assert_eq!(ed.message, "");

    feed(&mut ed, "n");
    assert_eq!(ed.cursor, Cursor::new(1, 0), "the next one");
    feed(&mut ed, "n");
    assert_eq!(ed.cursor, Cursor::new(0, 4), "and round again");
    feed(&mut ed, "N");
    assert_eq!(ed.cursor, Cursor::new(1, 0), "`N` the other way");

    feed(&mut ed, "?one<Enter>");
    assert_eq!(ed.cursor, Cursor::new(0, 8), "`?` searches backwards");
    feed(&mut ed, "n");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "and `n` keeps going that way");
    feed(&mut ed, "N");
    assert_eq!(ed.cursor, Cursor::new(0, 8), "`N` turns round");
}

#[test]
fn a_search_shows_its_match_while_the_pattern_is_typed() {
    // `'incsearch'`: the cursor goes to the match as the pattern is typed,
    // and each keystroke searches from where the prompt was opened again —
    // which is why a longer pattern narrows to a later match instead of
    // walking through them.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("xay\nxbz\n");
    feed(&mut ed, "/x");
    assert_eq!(ed.mode, Mode::Command, "still on the `:` line");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "the first `x`");
    feed(&mut ed, "b");
    assert_eq!(ed.cursor, Cursor::new(1, 0), "`xb` only matches below");

    // Accepting lands where the preview was, not on the match after it.
    feed(&mut ed, "<Enter>");
    assert_eq!(ed.mode, Mode::Normal);
    assert_eq!(ed.cursor, Cursor::new(1, 0));
    assert_eq!(ed.message, "");
    assert_eq!(ed.search.pattern(), "xb");
}

#[test]
fn a_cancelled_search_puts_the_cursor_and_the_search_back() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "/two<Enter>");
    assert_eq!(ed.cursor, Cursor::new(0, 4));

    feed(&mut ed, "/one");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "the preview moved");
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.cursor, Cursor::new(0, 4), "and the cursor came back");
    assert_eq!(
        ed.search.pattern(),
        "two",
        "a cancelled search puts the one that was in force back"
    );

    // With nothing in force before it, cancelling leaves no search at all.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "/two<Esc>");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    assert!(ed.search.is_empty(), "nothing to repeat");
    feed(&mut ed, "n");
    assert_eq!(ed.message, "E35: No previous search pattern");
}

#[test]
fn a_preview_that_finds_nothing_says_nothing() {
    // The search has not been run yet, so a pattern that matches nothing is
    // not an error: it shows nothing and says nothing, and the cursor stays
    // where the search started.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, "/zzz");
    assert_eq!(ed.message, "");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    assert!(ed.search.matches().is_empty(), "nothing is highlighted");

    // Nor is a pattern that does not compile — a half-typed one is normal.
    feed(&mut ed, "<Esc>");
    feed(&mut ed, "/(unclosed");
    assert_eq!(ed.message, "");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    feed(&mut ed, "<Esc>");

    // The line itself still reports both, when it is accepted.
    feed(&mut ed, "/zzz<Enter>");
    assert_eq!(ed.message, "E486: Pattern not found: zzz");
    feed(&mut ed, "/(unclosed<Enter>");
    assert!(ed.message.starts_with("E54: "), "{}", ed.message);
}

#[test]
fn a_search_finds_the_part_of_a_match_the_pattern_reports() {
    // `\zs` and `\ze` move the ends of a match, and the cursor goes to the
    // match the pattern reports rather than to the text the engine matched.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("alpha beta\n");
    feed(&mut ed, "/alpha \\zsbeta<Enter>");
    assert_eq!(ed.search.matches(), &[(6, 10)], "the part highlighted");
    assert_eq!(ed.cursor, Cursor::new(0, 6), "and where the cursor goes");
}

#[test]
fn noincsearch_types_the_pattern_without_moving() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    ed.execute_cmdline("set noincsearch");
    feed(&mut ed, "/two");
    assert_eq!(
        ed.cursor,
        Cursor::new(0, 0),
        "nothing is shown while typing"
    );
    feed(&mut ed, "<Enter>");
    assert_eq!(ed.cursor, Cursor::new(0, 4), "and the search still runs");
}

#[test]
fn ctrl_g_and_ctrl_t_walk_the_matches_being_typed() {
    // `<C-g>` goes to the next match and `<C-t>` to the previous one, while
    // the pattern is still on the line — vim's rule for them.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two one\n");
    feed(&mut ed, "/o");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    feed(&mut ed, "<C-g>");
    assert_eq!(ed.cursor, Cursor::new(0, 6), "the next `o`");
    feed(&mut ed, "<C-g>");
    assert_eq!(ed.cursor, Cursor::new(0, 8));
    feed(&mut ed, "<C-t>");
    assert_eq!(ed.cursor, Cursor::new(0, 6), "and back");
    // Typing again searches from the start once more.
    feed(&mut ed, "n");
    assert_eq!(
        ed.cursor,
        Cursor::new(0, 0),
        "`on` matches at the first `o`"
    );
    feed(&mut ed, "<C-g>");
    assert_eq!(ed.cursor, Cursor::new(0, 8), "the second `on`");
    feed(&mut ed, "<C-t>");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "and back to the first");
    feed(&mut ed, "<Enter>");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "accepted at the first match");
}

#[test]
fn a_search_that_finds_nothing_says_so_and_stays_put() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "/nope<Enter>");
    assert_eq!(ed.message, "E486: Pattern not found: nope");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    feed(&mut ed, "n");
    assert_eq!(ed.message, "E486: Pattern not found: nope");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
}

#[test]
fn an_empty_pattern_repeats_the_last_search() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "/<Enter>");
    assert_eq!(ed.message, "E35: No previous search pattern");
    feed(&mut ed, "n");
    assert_eq!(ed.message, "E35: No previous search pattern");

    feed(&mut ed, "/two<Enter>");
    feed(&mut ed, "0");
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    feed(&mut ed, "/<Enter>");
    assert_eq!(ed.cursor, Cursor::new(0, 4), "the same pattern again");
}

#[test]
fn escape_abandons_the_prompt_and_keeps_the_search() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "/two<Enter>");
    feed(&mut ed, "/no<Esc>");
    assert_eq!(ed.mode, Mode::Normal);
    assert_eq!(ed.cmdline, "", "the half-typed pattern is gone");
    assert_eq!(ed.search.pattern(), "two", "the search that was run is not");
    feed(&mut ed, "n");
    assert_eq!(ed.message, "", "and `n` still works");
}

#[test]
fn every_match_on_screen_is_highlighted() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two one\n");
    feed(&mut ed, "/one<Enter>");
    let out = ed.compose();
    assert_eq!(
        out.grid.get(0, 0).attrs & ATTR_UNDERLINE,
        ATTR_UNDERLINE,
        "the match is underlined"
    );
    assert_eq!(
        out.grid.get(0, 0).attrs & ATTR_BOLD,
        ATTR_BOLD,
        "and the one the cursor is on is bold too"
    );
    assert_eq!(out.grid.get(8, 0).attrs & ATTR_UNDERLINE, ATTR_UNDERLINE);
    assert_eq!(
        out.grid.get(8, 0).attrs & ATTR_BOLD,
        0,
        "the other one is not"
    );
    assert_eq!(
        out.grid.get(3, 0).attrs & ATTR_UNDERLINE,
        0,
        "the gap is not"
    );
}

#[test]
fn a_search_follows_the_text_after_an_edit() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "/two<Enter>");
    assert_eq!(ed.cursor, Cursor::new(0, 4));

    feed(&mut ed, "0iX<Esc>");
    assert_eq!(
        ed.search.matches(),
        &[(5, 8)],
        "the match moved with the text it is in"
    );
    feed(&mut ed, "n");
    assert_eq!(ed.cursor, Cursor::new(0, 5), "and `n` still lands on it");
}

#[test]
fn a_search_ignores_case_when_the_setting_says_so() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("One two\n");
    feed(&mut ed, "/one<Enter>");
    assert_eq!(ed.message, "E486: Pattern not found: one");

    ed.options.borrow_mut().ignorecase = true;
    feed(&mut ed, "/one<Enter>");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "`ignorecase` finds it now");
}

#[test]
fn searches_and_commands_keep_separate_histories() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "/two<Enter>");
    feed(&mut ed, ":<Up>");
    assert_eq!(ed.cmdline, "", "no command has been run yet");

    feed(&mut ed, "<Esc>");
    feed(&mut ed, "/<Up>");
    assert_eq!(ed.cmdline, "two", "the last pattern is there");
}

#[test]
fn star_searches_for_the_word_under_the_cursor() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two one\ntwo one\n");
    feed(&mut ed, "*");
    assert_eq!(ed.cursor, Cursor::new(0, 8), "the next `one`, not this one");
    feed(&mut ed, "n");
    assert_eq!(ed.cursor, Cursor::new(1, 4), "and `n` carries on");
    feed(&mut ed, "#");
    assert_eq!(ed.cursor, Cursor::new(0, 8), "and `#` goes back");
    assert_eq!(ed.message, "", "no complaint");

    // It picks up the word under the cursor and searches for that word
    // *whole*, not for its letters inside a longer one.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one someone one\n");
    feed(&mut ed, "*");
    assert_eq!(
        ed.cursor,
        Cursor::new(0, 12),
        "not the `one` inside `someone`"
    );

    // On whitespace it takes the word after the cursor, and a word there is
    // not one says so instead of searching for something odd.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "lll*"); // on the space, which is not a word
    assert_eq!(ed.cursor, Cursor::new(0, 4), "the word after the space");
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("   \n");
    feed(&mut ed, "*");
    assert_eq!(ed.message, "E348: No string under cursor");
}

#[test]
fn a_substitute_can_name_the_lines_it_looks_at() {
    let text = "one\ntwo one\nthree one\nfour\n";
    let run = |line: &str, start: usize| {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(text);
        feed(&mut ed, &format!("{}G", start + 1));
        ed.execute_cmdline(line);
        (ed.buffer.text(), ed.message)
    };

    // A pair of line numbers.
    let (out, message) = run("2,3s/one/ONE/", 0);
    assert_eq!(out, "one\ntwo ONE\nthree ONE\nfour\n");
    assert_eq!(message, "2 substitutions on 2 lines");

    // One number: that line and no other.
    let (out, _) = run("3s/one/ONE/", 0);
    assert_eq!(out, "one\ntwo one\nthree ONE\nfour\n");

    // `.` is where the cursor is, `$` the last line, and `.` to `$` is both.
    let (out, _) = run(".,$s/one/ONE/", 1);
    assert_eq!(out, "one\ntwo ONE\nthree ONE\nfour\n");
    let (out, _) = run("$s/one/ONE/", 0);
    assert_eq!(
        out, "one\ntwo one\nthree one\nfour\n",
        "the last line has none"
    );
    let (out, _) = run("1,2s/e/E/g", 0);
    assert_eq!(out, "onE\ntwo onE\nthree one\nfour\n");

    // Backwards is the same range as forwards, the way vim reads it.
    let (out, _) = run("3,2s/one/ONE/", 0);
    assert_eq!(out, "one\ntwo ONE\nthree ONE\nfour\n");

    // `%` is still the whole file, and no range at all is still this line.
    let (out, message) = run("%s/one/ONE/g", 2);
    assert_eq!(out, "ONE\ntwo ONE\nthree ONE\nfour\n");
    assert_eq!(message, "3 substitutions on 3 lines");
    let (out, _) = run("s/one/ONE/", 2);
    assert_eq!(out, "one\ntwo one\nthree ONE\nfour\n");
}

#[test]
fn a_bad_pattern_in_a_range_is_still_a_bad_pattern() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.execute_cmdline("1,2s/[/x/");
    assert!(ed.message.starts_with("E54:"), "{}", ed.message);
    assert_eq!(ed.buffer.text(), "one\ntwo\n");
}

#[test]
fn a_substitute_with_the_c_flag_asks_about_every_match() {
    // `y` replaces, `n` leaves alone, and the questions come in order.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one one\ntwo one\n");
    ed.execute_cmdline("%s/one/1/gc");
    assert!(ed.confirm.is_some(), "the first question is up");
    assert_eq!(ed.message, "replace with 1 (y/n/a/q/l)?");
    feed(&mut ed, "y");
    assert_eq!(ed.buffer.text(), "1 one\ntwo one\n");
    feed(&mut ed, "n");
    assert_eq!(ed.buffer.text(), "1 one\ntwo one\n", "`n` leaves it");
    feed(&mut ed, "y");
    assert_eq!(ed.buffer.text(), "1 one\ntwo 1\n");
    assert!(ed.confirm.is_none(), "the questions are over");
    assert_eq!(ed.message, "2 substitutions on 2 lines");

    // And the whole conversation is one undo step.
    feed(&mut ed, "u");
    assert_eq!(
        ed.buffer.text(),
        "one one\ntwo one\n",
        "one `u` takes it all back"
    );
}

#[test]
fn the_answers_of_a_confirmed_substitute() {
    // `a` takes this one and every one after it.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one one one\n");
    ed.execute_cmdline("s/one/1/gc");
    feed(&mut ed, "a");
    assert_eq!(ed.buffer.text(), "1 1 1\n");
    assert!(ed.confirm.is_none());
    assert_eq!(ed.message, "3 substitutions on 1 line");

    // `q` stops; `l` takes this one and stops.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one one one\n");
    ed.execute_cmdline("s/one/1/gc");
    feed(&mut ed, "q");
    assert_eq!(ed.buffer.text(), "one one one\n");
    assert!(ed.confirm.is_none());
    assert_eq!(ed.message, "0 substitutions on 0 lines");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one one one\n");
    ed.execute_cmdline("s/one/1/gc");
    feed(&mut ed, "l");
    assert_eq!(ed.buffer.text(), "1 one one\n");
    assert!(ed.confirm.is_none());

    // Escape stops as well, and a key that is not an answer is ignored.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    ed.execute_cmdline("s/one/1/c");
    feed(&mut ed, "z");
    assert_eq!(ed.message, "replace with 1 (y/n/a/q/l)?", "still asking");
    feed(&mut ed, "<Esc>");
    assert!(ed.confirm.is_none());
    assert_eq!(ed.buffer.text(), "one\n");

    // With nothing to ask about there is nothing to confirm.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("nothing here\n");
    ed.execute_cmdline("s/one/1/c");
    assert!(ed.confirm.is_none());
    assert_eq!(ed.message, "E486: Pattern not found: one");
}

#[test]
fn a_substitute_replaces_on_the_cursor_line() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two one\ntwo\n");
    feed(&mut ed, ":s/one/1<Enter>");
    assert_eq!(
        ed.buffer.text(),
        "1 two one\ntwo\n",
        "only the first, and only this line"
    );
    assert_eq!(ed.message, "1 substitution on 1 line");

    feed(&mut ed, ":s/one/1/g<Enter>");
    assert_eq!(ed.buffer.text(), "1 two 1\ntwo\n", "`g` takes them all");
    assert_eq!(ed.message, "1 substitution on 1 line", "the one left");
}

#[test]
fn a_whole_file_substitute_is_one_undo_step() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\none three\nfour\n");
    feed(&mut ed, ":%s/one/1/g<Enter>");
    assert_eq!(ed.buffer.text(), "1 two\n1 three\nfour\n");
    assert_eq!(ed.message, "2 substitutions on 2 lines");
    assert_eq!(
        ed.cursor.row, 1,
        "the cursor ends on the last line that changed"
    );

    feed(&mut ed, "u");
    assert_eq!(
        ed.buffer.text(),
        "one two\none three\nfour\n",
        "one `u` puts the whole file back"
    );
}

#[test]
fn a_substitute_can_add_at_the_start_of_every_line() {
    // A match of nothing is not a match for a search, but it is one for a
    // substitution: `^` alone indents a file.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    feed(&mut ed, ":%s/^/  /<Enter>");
    assert_eq!(ed.buffer.text(), "  one\n  two\n");
    assert_eq!(ed.message, "2 substitutions on 2 lines");
}

#[test]
fn a_substitute_that_finds_nothing_says_so() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, ":s/xyz/1/<Enter>");
    assert_eq!(ed.message, "E486: Pattern not found: xyz");
    assert_eq!(ed.buffer.text(), "one\n");
    assert!(!ed.is_dirty(), "and nothing was written down");

    feed(&mut ed, ":s/(unclosed/1/<Enter>");
    assert!(ed.message.starts_with("E54: "), "{}", ed.message);
    assert!(!ed.is_dirty());

    // A pattern that asks for something this cannot do is refused, and named.
    // The refusal is already an `E54` — it must not say `E54: E: …`.
    feed(&mut ed, ":s/\\zs*/x/<Enter>");
    assert_eq!(ed.message, "E54: \\zs cannot be followed by a multi");
    assert!(!ed.is_dirty());
    feed(&mut ed, ":s/[\\zs]/x/<Enter>");
    assert_eq!(
        ed.message,
        "E54: \\zs is not supported inside a collection: vim reads it there as characters"
    );
    assert!(!ed.is_dirty());
}

#[test]
fn a_refused_pattern_says_which_one_and_why() {
    // The same refusal reaches a person through `/` and through `:s`, and
    // both should say the same thing: the code, the thing that is not
    // supported, and the reason.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    feed(&mut ed, "/[\\zs]<Enter>");
    assert_eq!(
        ed.message,
        "E54: \\zs is not supported inside a collection: vim reads it there as characters"
    );
}

#[test]
fn the_pattern_of_a_substitute_is_a_regular_expression() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("a1 b22 c333\n");
    feed(&mut ed, ":s/\\d+/N/g<Enter>");
    assert_eq!(ed.buffer.text(), "aN bN cN\n");
    assert_eq!(ed.message, "3 substitutions on 1 line");
}

#[test]
fn a_substitute_leaves_a_program_alone_when_it_should() {
    // The delimiter is what lets a pattern hold a slash, and a `$` in a
    // replacement is a dollar sign rather than the engine's own spelling.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("path /usr/bin and cost $5\n");
    feed(&mut ed, ":s|/usr/bin|/opt/bin|<Enter>");
    assert_eq!(ed.buffer.text(), "path /opt/bin and cost $5\n");
    feed(&mut ed, ":s|cost|price|<Enter>");
    assert_eq!(ed.buffer.text(), "path /opt/bin and price $5\n");
}

#[test]
fn a_substitute_refreshes_the_search() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "/two<Enter>");
    assert_eq!(ed.search.matches(), &[(4, 7)]);
    feed(&mut ed, ":%s/two/2/<Enter>");
    assert_eq!(ed.buffer.text(), "one 2\n");
    assert!(
        ed.search.matches().is_empty(),
        "the text it was looking for is gone"
    );
}

#[test]
fn no_hlsearch_stops_drawing_the_matches_until_a_search_is_used() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\nthree two\n");
    feed(&mut ed, "/two<Enter>");
    assert_eq!(ed.view().matches.len(), 2, "both are highlighted");
    assert!(
        ed.view().current_match.is_some(),
        "and this one is stronger"
    );

    // `:nohlsearch` is about the display, not the setting: the pattern stays,
    // and the next search brings the highlighting back.
    feed(&mut ed, ":nohlsearch<Enter>");
    assert!(ed.view().matches.is_empty(), "nothing is drawn");
    assert!(ed.view().current_match.is_none());
    assert!(!ed.search.is_empty(), "but the search is still up");
    feed(&mut ed, "n");
    assert_eq!(ed.view().matches.len(), 2, "using a search brings it back");
    // The setting was never touched, so `:set hlsearch?` still says on.
    feed(&mut ed, ":set hlsearch?<Enter>");
    assert_eq!(ed.message, "hlsearch=true");

    // `:set nohlsearch` is the setting itself, and stays off.
    feed(&mut ed, ":set nohlsearch<Enter>");
    assert!(ed.view().matches.is_empty());
    feed(&mut ed, "n");
    assert!(ed.view().matches.is_empty(), "and `n` does not light them");
    feed(&mut ed, ":set hlsearch<Enter>");
    assert_eq!(ed.view().matches.len(), 2, "until it is set again");
}

#[test]
fn no_wrapscan_reports_that_there_is_nothing_past_the_last_match() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    feed(&mut ed, "/two<Enter>");
    assert_eq!(ed.cursor, Cursor::new(0, 4));

    // With the default `'wrapscan'`, `n` comes round again.
    feed(&mut ed, "n");
    assert_eq!(ed.cursor, Cursor::new(0, 4), "the same match, wrapped");
    assert_eq!(ed.message, "");

    feed(&mut ed, ":set nowrapscan<Enter>");
    feed(&mut ed, "n");
    assert_eq!(
        ed.message, "E486: Pattern not found: two",
        "there is nothing after the last match, and now it says so"
    );
    assert_eq!(ed.cursor, Cursor::new(0, 4), "and it stays where it was");
    feed(&mut ed, "N");
    assert_eq!(ed.message, "E486: Pattern not found: two", "either way");

    // A fresh search obeys it too.
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "/one<Enter>");
    assert_eq!(ed.cursor, Cursor::new(0, 0), "found where it is");
    feed(&mut ed, "n");
    assert_eq!(ed.message, "E486: Pattern not found: one");
}

#[test]
fn normal_leaves_what_its_keys_had_to_say_standing() {
    // `:normal` runs keys as if typed, so a key that fails says why: a count of
    // lines normalised would cover the one thing the person needs to read.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one two\n");
    // The keys a `:normal` runs start at the beginning of the line, so the
    // failing command in this test is a search with nothing to find.
    feed(&mut ed, ":normal /zzz<Enter><Enter>");
    assert_eq!(ed.message, "E486: Pattern not found: zzz");

    // With nothing to say, the count is what shows.
    feed(&mut ed, ":normal x<Enter>");
    assert_eq!(ed.message, "1 line normalised");
}

#[test]
fn smartcase_takes_ignorecase_back_for_a_pattern_with_a_capital() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("Hello hello\n");
    ed.execute_cmdline("set ignorecase");
    feed(&mut ed, "/HELLO<Enter>");
    assert_eq!(
        ed.search.matches().len(),
        2,
        "'ignorecase' alone finds both"
    );
    feed(&mut ed, "/Hello<Enter>");
    assert_eq!(
        ed.search.matches().len(),
        2,
        "and a capital changes nothing"
    );

    // With `'smartcase'`, a pattern with an upper case letter in it is taken
    // literally: it is what makes `/Foo` find `Foo` and `/foo` find either.
    ed.execute_cmdline("set smartcase");
    feed(&mut ed, "/HELLO<Enter>");
    assert_eq!(ed.message, "E486: Pattern not found: HELLO");
    feed(&mut ed, "/Hello<Enter>");
    assert_eq!(ed.search.matches().len(), 1, "the capital one only");
    feed(&mut ed, "/hello<Enter>");
    assert_eq!(
        ed.search.matches().len(),
        2,
        "all lower case still finds both"
    );
}

#[test]
fn a_substitute_obeys_the_case_settings_and_its_own_flags() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("Hello hello\n");
    // `'ignorecase'` is used for `:s` as much as for `/`.
    ed.execute_cmdline("set ignorecase");
    ed.execute_cmdline("s/HELLO/x/");
    assert_eq!(ed.buffer.text(), "x hello\n");

    // `'smartcase'` takes it back for a pattern with an upper case letter.
    ed.buffer = Buffer::from_text("Hello hello\n");
    ed.execute_cmdline("set smartcase");
    ed.execute_cmdline("s/HELLO/x/");
    assert_eq!(ed.buffer.text(), "Hello hello\n", "nothing was replaced");
    assert_eq!(ed.message, "E486: Pattern not found: HELLO");
    // And the `i` flag is the way to say "ignore case here", `I` the way to say
    // the opposite when the setting would have ignored it.
    ed.execute_cmdline("s/HELLO/x/i");
    assert_eq!(ed.buffer.text(), "x hello\n");
    ed.buffer = Buffer::from_text("Hello hello\n");
    ed.execute_cmdline("s/HELLO/y/I");
    assert_eq!(ed.buffer.text(), "Hello hello\n", "`I` is case matters");
}

#[test]
fn a_global_line_obeys_the_case_settings() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("Hello\nhello\nbye\n");
    ed.execute_cmdline("set ignorecase");
    ed.execute_cmdline("g/HELLO/d");
    assert_eq!(ed.buffer.text(), "bye\n", "both lines went");

    ed.buffer = Buffer::from_text("Hello\nhello\nbye\n");
    ed.execute_cmdline("set smartcase");
    ed.execute_cmdline("g/HELLO/d");
    assert_eq!(
        ed.buffer.text(),
        "Hello\nhello\nbye\n",
        "the capital is taken literally, and nothing matches it"
    );
    ed.execute_cmdline("g/Hello/d");
    assert_eq!(
        ed.buffer.text(),
        "hello\nbye\n",
        "and this one line matches"
    );
}
