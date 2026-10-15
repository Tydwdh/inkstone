//! `:vimgrep` — the list of every place a pattern matches in a set of files.

use super::*;

/// A directory of its own for a test, with the files it needs in it.
///
/// The name says which test it belongs to: the directory outlives the run, so
/// two tests sharing one would read each other's files.
fn scratch(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("inkstone-vimgrep-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, text) in files {
        let path = dir.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, text).unwrap();
    }
    dir
}

#[test]
fn a_vimgrep_fills_the_list_and_goes_to_the_first_place() {
    let dir = scratch(
        "one",
        &[("a.txt", "one two one\nthree\n"), ("b.txt", "four\none\n")],
    );
    let a = dir.join("a.txt");
    let b = dir.join("b.txt");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("elsewhere\n");
    ed.execute_cmdline(&format!("vimgrep /one/ {} {}", a.display(), b.display()));
    assert_eq!(ed.places.len(), 2, "a line is a place once, without `g`");
    assert_eq!(
        (ed.places[0].0.clone(), ed.places[0].1, ed.places[0].2),
        (a.clone(), 0, 0)
    );
    assert_eq!(
        (ed.places[1].0.clone(), ed.places[1].1, ed.places[1].2),
        (b.clone(), 1, 0)
    );
    // Without `j` the first place is gone to, which opens the file.
    assert_eq!(ed.buffer.path(), Some(a.as_path()));
    assert_eq!(ed.cursor, Cursor::new(0, 0));
    assert_eq!(ed.message, format!("1 of 2: {}", a.display()));

    // And the list is the one `:copen` shows and `:cnext` walks.
    ed.execute_cmdline("copen");
    assert_eq!(ed.message, "2 places");
    assert_eq!(ed.list, Some(List::Places));
    ed.execute_cmdline("cnext");
    assert_eq!(ed.buffer.path(), Some(b.as_path()));
    assert_eq!(ed.cursor.row, 1);
}

#[test]
fn every_match_on_a_line_is_a_place_of_its_own_with_g() {
    let dir = scratch("g", &[("a.txt", "one two one\n")]);
    let a = dir.join("a.txt");

    let mut ed = Editor::for_tests();
    ed.cursor = Cursor::new(0, 3);
    ed.execute_cmdline(&format!("vimgrep /one/gj {}", a.display()));
    assert_eq!(ed.places.len(), 2, "`g`: every match");
    assert_eq!((ed.places[0].1, ed.places[0].2), (0, 0));
    assert_eq!((ed.places[1].1, ed.places[1].2), (0, 8), "the second `one`");
    // `j` is "only the quickfix list is updated": no file was opened and the
    // cursor did not move.
    assert_eq!(ed.buffer.path(), None);
    assert_eq!(ed.cursor, Cursor::new(0, 3));
    assert_eq!(ed.message, "2 matches in 1 file");

    // The column is the one the pattern *reports*, which is what `\zs` moves —
    // vim records the same one.
    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("vimgrep /o\\zs/j {}", a.display()));
    assert_eq!((ed.places[0].1, ed.places[0].2), (0, 1), "after the `o`");
}

#[test]
fn the_pattern_a_vimgrep_takes_is_a_search_pattern() {
    let dir = scratch("pattern", &[("a.txt", "ONE\none\nTODO here\n")]);
    let a = dir.join("a.txt");

    // Case: `\c` and `\C` decide it, `'ignorecase'` is what a pattern that says
    // nothing gets — and `'smartcase'` is *not* used, which the manual says of
    // this command alone.
    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("vimgrep /one/j {}", a.display()));
    assert_eq!(ed.places.len(), 1, "the lower case one only");
    ed.execute_cmdline(&format!("vimgrep /\\cone/j {}", a.display()));
    assert_eq!(ed.places.len(), 2, "`\\c` ignores case");
    ed.execute_cmdline(&format!("vimgrep /\\Cone/j {}", a.display()));
    assert_eq!(ed.places.len(), 1);
    ed.options.borrow_mut().ignorecase = true;
    ed.options.borrow_mut().smartcase = true;
    ed.execute_cmdline(&format!("vimgrep /ONE/j {}", a.display()));
    assert_eq!(
        ed.places.len(),
        2,
        "`smartcase` would have taken `ignorecase` back for a search"
    );

    // The dialect is the search one, `\zs` and the classes included.
    ed.execute_cmdline(&format!("vimgrep /TODO\\zs/j {}", a.display()));
    assert_eq!(ed.places.len(), 1);
    assert_eq!(ed.places[0].1, 2, "the line the `TODO` is on");
    ed.execute_cmdline(&format!("vimgrep /\\k\\+/j {}", a.display()));
    assert_eq!(ed.places.len(), 3, "one name on each line");
}

#[test]
fn the_pattern_may_be_a_word_and_the_files_may_be_wildcards() {
    let dir = scratch(
        "files",
        &[
            ("x.rs", "let needle = 1;\n"),
            ("y.txt", "needle\n"),
            ("sub/z.rs", "// needle\n"),
            ("sub/w.txt", "needle\n"),
        ],
    );

    // A pattern that starts with an identifier character needs no delimiters.
    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("vimgrep needle {}/*.rs", dir.display()));
    assert_eq!(ed.places.len(), 1, "the Rust file beside the pattern");
    assert_eq!(ed.places[0].0, dir.join("x.rs"));

    // `**` is this directory and the ones under it, so the name may be a
    // pattern too — which is the difference between this and `gf`.
    ed.execute_cmdline(&format!("vimgrep /needle/j {}/**/*.rs", dir.display()));
    let mut found: Vec<String> = ed
        .places
        .iter()
        .map(|place| place.0.display().to_string())
        .collect();
    let mut expected = vec![
        dir.join("sub").join("z.rs").display().to_string(),
        dir.join("x.rs").display().to_string(),
    ];
    // The walk finds them in name order, which is `sub` before `x`.
    found.sort();
    expected.sort();
    assert_eq!(found, expected);

    // A file that is not there is not an error of its own: nothing matched.
    ed.execute_cmdline(&format!(
        "vimgrep /needle/ {}",
        dir.join("nope.rs").display()
    ));
    assert_eq!(ed.places.len(), 0);
    assert!(
        ed.message.starts_with("E480: No match: needle"),
        "{}",
        ed.message
    );
}

#[test]
fn a_count_is_the_most_places_to_find() {
    let dir = scratch("count", &[("a.txt", "one one one one\n")]);
    let a = dir.join("a.txt");

    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("1vimgrep /one/gj {}", a.display()));
    assert_eq!(ed.places.len(), 1, "the first one only");
    ed.execute_cmdline(&format!("3vimgrep /one/gj {}", a.display()));
    assert_eq!(ed.places.len(), 3);
    // The number is a count, not a range: vim refuses the comma and so does
    // this, rather than reading the `5` as the count.
    ed.execute_cmdline(&format!("1,5vimgrep /one/j {}", a.display()));
    assert_eq!(
        ed.message, "E481: No range allowed",
        "a range would mean something else entirely"
    );
}

#[test]
fn wildignore_keeps_a_file_out_of_a_search() {
    let dir = scratch(
        "ignore",
        &[
            ("keep.txt", "needle\n"),
            ("skip.txt", "needle\n"),
            ("target/inner.rs", "needle\n"),
        ],
    );
    let names = |ed: &Editor| -> Vec<String> {
        ed.places
            .iter()
            .map(|place| {
                place
                    .0
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };

    // A pattern with no separator in it is the file's name, and a file that
    // matches is ignored whether a wildcard found it or the argument named it.
    let mut ed = Editor::for_tests();
    ed.options.borrow_mut().wildignore = "skip.txt".to_string();
    ed.execute_cmdline(&format!("vimgrep /needle/j {}/**", dir.display()));
    assert_eq!(names(&ed), vec!["keep.txt", "inner.rs"], "skip.txt is out");
    ed.execute_cmdline(&format!("vimgrep /needle/j {}/skip.txt", dir.display()));
    assert_eq!(ed.places.len(), 0, "and named as it is, the same");
    assert!(ed.message.starts_with("E480"), "{}", ed.message);

    // A pattern with a separator in it is matched against the path, which is
    // what keeps a build tree out of a search.
    ed.options.borrow_mut().wildignore = format!("{}/*", dir.join("target").display());
    ed.execute_cmdline(&format!("vimgrep /needle/j {}/**", dir.display()));
    assert_eq!(names(&ed), vec!["keep.txt", "skip.txt"]);
}

#[test]
fn a_suffixed_file_is_searched_last() {
    let dir = scratch(
        "suffixes",
        &[
            ("a.zz", "needle\n"),
            ("b.txt", "needle\n"),
            ("keep.txt", "needle\n"),
        ],
    );
    let names = |ed: &Editor| -> Vec<String> {
        ed.places
            .iter()
            .map(|place| {
                place
                    .0
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };

    let mut ed = Editor::for_tests();
    ed.options.borrow_mut().suffixes = ".zz".to_string();
    ed.execute_cmdline(&format!("vimgrep /needle/j {}/*", dir.display()));
    assert_eq!(
        names(&ed),
        vec!["b.txt", "keep.txt", "a.zz"],
        "the suffix the setting names comes last"
    );

    // The default list is vim's, so a backup is searched last without anybody
    // asking: the terminal says `suffixes=.bak,~,.o,.h,.info,.swp,.obj`.
    let dir = scratch(
        "suffixes-default",
        &[("a.bak", "needle\n"), ("b.txt", "needle\n")],
    );
    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("vimgrep /needle/j {}/*", dir.display()));
    assert_eq!(names(&ed), vec!["b.txt", "a.bak"]);
}

#[test]
fn a_fuzzy_pattern_matches_characters_in_order() {
    let dir = scratch(
        "fuzzy",
        &[
            (
                "a.txt",
                "README\nread_me\nr a d m e\nno match here\nxxREADME again\n",
            ),
            ("dots.txt", "ax.c\nabc\n"),
        ],
    );
    let a = dir.join("a.txt");

    let mut ed = Editor::for_tests();
    ed.execute_cmdline(&format!("vimgrep /rdme/fj {}", a.display()));
    // The lines and the columns are vim's own answers, from `getqflist()`: the
    // fuzzy match is the *best* one in the line, not the leftmost.
    assert_eq!(
        ed.places
            .iter()
            .map(|place| (place.1 + 1, place.2 + 1))
            .collect::<Vec<_>>(),
        vec![(1, 1), (2, 1), (3, 1), (5, 3)],
        "README, read_me, `r a d m e`, and the one after `xx`"
    );
    assert_eq!(ed.message, "4 matches in 1 file");

    // `g` asks for every match on a line, and a fuzzy pattern has one: the best
    // one. So it changes nothing here.
    ed.execute_cmdline(&format!("vimgrep /rdme/fgj {}", a.display()));
    assert_eq!(ed.places.len(), 4);

    // A capital in the pattern wants a capital: `Rdme` finds `README` and not
    // `read_me`, which is the rule the terminal showed.
    ed.execute_cmdline(&format!("vimgrep /Rdme/fj {}", a.display()));
    assert_eq!(
        ed.places.iter().map(|place| place.1).collect::<Vec<_>>(),
        vec![0, 4],
        "the two lines with a capital R"
    );

    // The pattern is a *literal string* rather than a regular expression: the
    // `.` stands for itself, so `a.c` finds `ax.c` and not `abc`.
    let dots = dir.join("dots.txt");
    ed.execute_cmdline(&format!("vimgrep /a.c/fj {}", dots.display()));
    assert_eq!(ed.places.len(), 1, "one of the two lines");
    assert_eq!((ed.places[0].1, ed.places[0].2), (0, 0), "the `ax.c`");
    // hmm: "abc" would match if the dot were "any character".
    ed.execute_cmdline(&format!("vimgrep /a.c/j {}", dots.display()));
    assert_eq!(ed.places.len(), 1, "and the dot is still a dot");
}

#[test]
fn an_empty_pattern_is_the_last_search_and_the_last_search_is_not_replaced() {
    let dir = scratch("last", &[("a.txt", "one\nthree\n")]);
    let a = dir.join("a.txt");

    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    ed.execute_cmdline(&format!("vimgrep //j {}", a.display()));
    assert_eq!(ed.message, "E35: No previous search pattern");

    feed(&mut ed, "/one<Enter>");
    ed.execute_cmdline(&format!("vimgrep //j {}", a.display()));
    assert_eq!(ed.places.len(), 1, "the last search pattern was `one`");

    // The pattern a `:vimgrep` was given is not the last search pattern: vim
    // keeps its own, and a person notices when they have typed both.
    ed.execute_cmdline(&format!("vimgrep /three/j {}", a.display()));
    assert_eq!(ed.places.len(), 1);
    assert_eq!(ed.search.pattern(), "one", "still the search's own");
}
