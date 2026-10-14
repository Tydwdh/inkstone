use super::*;

#[test]
fn smartindent_opens_a_level_and_a_brace_gives_one_back() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("fn main() {\n    let x = 1;\n}\n");
    ed.options.borrow_mut().smartindent = true;
    // A line after an opener gets one more level, on top of the line's own.
    ed.cursor = Cursor::new(0, 10);
    feed(&mut ed, "o");
    assert_eq!(
        ed.buffer.text(),
        "fn main() {\n\t\n    let x = 1;\n}\n",
        "the `{{` opened a level"
    );
    assert_eq!(ed.cursor, Cursor::new(1, 1), "after the indentation");
    feed(&mut ed, "<Esc>");
    // A line after a plain one gets the line's own indentation only.
    ed.buffer = Buffer::from_text("fn main() {\n    let x = 1;\n}\n");
    ed.cursor = Cursor::new(1, 0);
    feed(&mut ed, "o");
    assert_eq!(ed.buffer.text(), "fn main() {\n    let x = 1;\n    \n}\n");
    feed(&mut ed, "<Esc>");
    // `}` typed where a line has nothing but indentation takes a level back.
    ed.buffer = Buffer::from_text("fn main() {\n    \t\n}\n");
    ed.cursor = Cursor::new(1, 0);
    feed(&mut ed, "A");
    assert_eq!(ed.cursor.col, 5, "at the end of the indentation");
    ed.type_text("}");
    assert_eq!(ed.buffer.text(), "fn main() {\n    }\n}\n");
    feed(&mut ed, "<Esc>");
    // With spaces it takes off one tab stop's worth.
    ed.buffer = Buffer::from_text("    x\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "A");
    ed.type_text("}");
    assert_eq!(ed.buffer.text(), "    x}\n", "not at the start of the line");
    // Both settings off: nothing is indented, and `}` is just a brace.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("    \n");
    ed.cursor = Cursor::new(0, 4);
    ed.type_text("}");
    assert_eq!(ed.buffer.text(), "    }\n");
}

#[test]
fn a_range_can_be_reindented_by_what_it_opens_and_closes() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("fn main() {\nlet x = 1;\nif x > 0 {\nprintln!(\"x\");\n}\n}\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "gg=G");
    assert_eq!(
        ed.buffer.text(),
        "fn main() {\n\tlet x = 1;\n\tif x > 0 {\n\t\tprintln!(\"x\");\n\t}\n}\n",
        "each line is as deep as what is open above it"
    );
    assert!(ed.message.contains("reindented"), "{}", ed.message);
    // One edit, one undo.
    feed(&mut ed, "u");
    assert_eq!(
        ed.buffer.text(),
        "fn main() {\nlet x = 1;\nif x > 0 {\nprintln!(\"x\");\n}\n}\n"
    );
    // `==` is one line, and a range that starts inside a block keeps the
    // depth it is in.
    ed.buffer = Buffer::from_text("fn main() {\n    \tlet x = 1;\n}\n");
    ed.cursor = Cursor::new(1, 0);
    feed(&mut ed, "==");
    assert_eq!(ed.buffer.text(), "fn main() {\n\tlet x = 1;\n}\n");
    // A blank line stays blank rather than becoming a line of tabs.
    ed.buffer = Buffer::from_text("fn main() {\n\t\t\n\tlet x = 1;\n}\n");
    ed.cursor = Cursor::new(1, 0);
    feed(&mut ed, "==");
    assert_eq!(ed.buffer.text(), "fn main() {\n\n\tlet x = 1;\n}\n");
    // With a count, and a motion that names several lines.
    ed.buffer = Buffer::from_text("a\n{\nb\nc\n}\n");
    ed.cursor = Cursor::new(0, 0);
    feed(&mut ed, "2==");
    assert_eq!(
        ed.buffer.text(),
        "a\n{\nb\nc\n}\n",
        "nothing was out of place"
    );
    feed(&mut ed, "G=gg");
    assert_eq!(ed.buffer.text(), "a\n{\n\tb\n\tc\n}\n");
}
