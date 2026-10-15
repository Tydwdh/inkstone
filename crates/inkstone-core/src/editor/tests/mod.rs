use super::*;
use crate::keymap::parse_key;

fn feed(editor: &mut Editor, notation: &str) {
    for key in parse_key(notation).unwrap() {
        editor.handle_key(key);
    }
}

mod folds;
mod indent;
mod insert;
mod keys;
mod lsp;
mod marks;
mod motions;
mod operators;
mod options;
mod prompt;
mod registers;
mod search;
mod undo;
mod vimgrep;
mod visual;
mod windows;
