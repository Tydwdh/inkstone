//! The command registry: every atomic editor operation, addressable by name.
//!
//! Commands are plain `fn(&mut Editor, &[Param]) -> Result<()>`. Keeping them
//! as functions (rather than closures) means the registry stays a flat table
//! that a script layer can enumerate and dispatch into, and means a command
//! can never capture editor state.
//!
//! Commands with a numeric first parameter take it as a repeat count, so `3x`
//! and `dd` (count 1) go through the same code path.

use std::collections::HashMap;
use std::path::Path;

use inkstone_rpc::Param;

use crate::buffer::Buffer;
use crate::cursor::Cursor;
use crate::editor::ForcedShape;
use crate::editor::{Ask, Axis, Editor, Object, Operator, Prompt, Rect, Resize, Selection};
use crate::keymap::Binding;
use crate::marks::MarkJump;
use crate::mode::{Mode, VisualKind};
use crate::register::{Kind, Register};
use crate::substitute::Substitute;

/// What a command returns: `Err` carries the message the user should see.
pub type Result<T, E = CommandError> = std::result::Result<T, E>;

/// Why a command did not run.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommandError {
    /// No command by that name, in the registry or in Lua.
    #[error("E492: Not a command: {0}")]
    Unknown(String),
    /// The command ran and refused, for example a write that failed. The message is
    /// shown to the user as-is.
    #[error("{0}")]
    Failed(String),
}

/// The shape of every built-in command.
///
/// Plain functions, not closures, which is why the registry stays a table a script
/// layer can enumerate and dispatch into.
pub(crate) type CommandFn = fn(&mut Editor, &[Param]) -> Result<()>;

/// The set of built-in commands.
#[derive(Debug, Clone)]
pub struct CommandRegistry {
    map: HashMap<&'static str, CommandFn>,
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

impl CommandRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    commands! {
        noop,
        move_left,
        move_right,
        move_up,
        move_down,
        move_word_forward,
        move_word_backward,
        move_line_start,
        move_line_end,
        goto_first_line,
        goto_last_line,
        fold_motion,
        fold_line,
        fold_open,
        fold_close,
        fold_open_all,
        fold_close_all,
        fold_delete,
        make_view,
        load_view,
        fold_level_less,
        fold_level_more,
        fold_update,
        fold_next,
        fold_previous,
        join,
        join_visual,
        join_tight,
        join_tight_visual,
        goto_file,
        command_history,
        search_history,
        block_to_end,
        visual_block,
        insert_block_left,
        insert_block_right,
        goto_screen_top,
        goto_screen_middle,
        goto_screen_bottom,
        scroll_line_down,
        scroll_line_up,
        half_page_down,
        half_page_up,
        page_up,
        page_down,
        enter_insert,
        enter_insert_before,
        append,
        append_eol,
        open_line_below,
        open_line_above,
        delete_char,
        delete_forwards,
        increment,
        decrement,
        toggle_case,
        match_bracket,
        delete_line,
        print_line,
        set_mark,
        mark_exact,
        mark_line,
        jump_back_line,
        jump_back_exact,
        jump_oldest,
        jump_newest,
        list_jumps,
        delete_marks,
        delete_all_marks,
        clear_jumps,
        change_older,
        change_newer,
        list_changes,
        list_marks,
        yank_line,
        change_line,
        delete_motion,
        yank_motion,
        change_motion,
        format_motion,
        reindent_motion,
        reindent_line,
        indent_motion,
        outdent_motion,
        indent_line,
        outdent_line,
        format_line,
        toggle_case_motion,
        lower_motion,
        upper_motion,
        toggle_case_line,
        lower_case_line,
        upper_case_line,
        put_after,
        put_before,
        copy,
        enter_visual,
        enter_visual_line,
        escape_visual,
        swap_ends,
        delete_visual,
        yank_visual,
        change_visual,
        upper_visual,
        lower_visual,
        toggle_case_visual,
        indent_visual,
        outdent_visual,
        reindent_visual,
        put_visual,
        undo,
        redo,
        enter_command,
        substitute,
        search_forward,
        search_backward,
        search_next,
        search_previous,
        escape,
        save,
        save_force,
        quit,
        save_quit,
        quit_force,
        edit_file,
        edit_file_force,
        search_word_forward,
        search_word_backward,
        repeat,
        substitute_again,
        substitute_again_flags,
        find_forward,
        find_backward,
        till_forward,
        till_backward,
        find_repeat,
        find_reverse,
        resize,
        resize_vertical,
        equalize_windows,
        next_diagnostic,
        previous_diagnostic,
        list_diagnostics,
        set_diagnostics,
        hover,
        goto_definition,
        goto_references,
        list_symbols,
        workspace_symbols,
        format_document,
        format_range_command,
        signature_help,
        place_next,
        place_previous,
        place_list,
        reselect,
        insert_again,
        open_places,
        close_places,
        list_older,
        list_newer,
        open_diagnostics,
        close_diagnostics,
        open_place,
        complete_next,
        complete_previous,
        complete_accept,
        complete_cancel,
        snippet_next,
        snippet_previous,
        snippet_choice_next,
        snippet_choice_previous,
        tab_new,
        tab_split,
        tab_close,
        tab_next,
        tab_previous,
        list_tabs,
        scroll_to_top,
        scroll_to_center,
        scroll_to_bottom,
        list_buffers,
        buffer_go,
        buffer_next,
        buffer_next_force,
        buffer_previous,
        buffer_previous_force,
        buffer_delete,
        buffer_delete_force,
        split_rows,
        split_columns,
        window_next,
        window_left,
        window_down,
        window_up,
        window_right,
        window_close,
        window_only,
        list_registers,
    }

    /// Install (or replace) a command.
    ///
    /// Built-ins go through the `commands!` macro instead, so the name and the
    /// function cannot disagree.
    pub fn register(&mut self, name: &'static str, f: CommandFn) {
        self.map.insert(name, f);
    }

    /// Whether a command by this name exists.
    pub fn contains(&self, name: &str) -> bool {
        self.map.contains_key(name)
    }

    /// The function for `name`, if registered.
    ///
    /// Returns the bare function pointer rather than a reference so that a
    /// caller holding `&Editor` can hand `&mut Editor` to the command.
    pub fn get(&self, name: &str) -> Option<CommandFn> {
        self.map.get(name).copied()
    }

    /// Run `name` against `editor`.
    pub fn run(&self, editor: &mut Editor, name: &str, params: &[Param]) -> Result<()> {
        match self.map.get(name) {
            Some(f) => f(editor, params),
            None => Err(CommandError::Unknown(name.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The largest count a command will honour.
///
/// A count larger than the buffer cannot do more than one that is not, and a
/// count of a billion would otherwise be a billion trips round a loop: the cap
/// makes `999999999j` behave like `100000j`, which is already further than any
/// buffer goes.
pub(crate) const MAX_COUNT: usize = 100_000;

/// The repeat count for a command: the first integer parameter, if positive.
fn count(params: &[Param]) -> usize {
    match params.first() {
        Some(Param::Int(n)) if *n > 0 => (*n as usize).min(MAX_COUNT),
        _ => 1,
    }
}

/// The motion a command was given, if it was given one: `d` with no motion
/// starts the operator, and the same command with one applies it.
fn motion_param(params: &[Param]) -> Option<&str> {
    params.iter().find_map(|param| match param {
        Param::Str(name) if !name.is_empty() => Some(name.as_str()),
        _ => None,
    })
}

/// The first string parameter, as a path.
fn str_param(params: &[Param]) -> Option<&str> {
    match params.first() {
        Some(Param::Str(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn path_param(params: &[Param]) -> Option<&Path> {
    match params.first() {
        Some(Param::Str(s)) if !s.is_empty() => Some(Path::new(s)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn noop(_editor: &mut Editor, _params: &[Param]) -> Result<()> {
    Ok(())
}

fn move_left(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_left(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn move_right(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_right(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

/// Which way a character find looks, and whether it lands on the character or
/// next to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Find {
    /// `f` — on the character, looking forward.
    Forward,
    /// `F` — on it, looking back.
    Backward,
    /// `t` — just before it, looking forward.
    Till,
    /// `T` — just after it, looking back.
    TillBackward,
}

/// What `<C-a>` and `<C-x>` do to the number they find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counter {
    /// Add the count.
    Add,
    /// Take it away.
    Subtract,
}

/// The number `<C-a>` and `<C-x>` work on: the one under the cursor, or the next
/// one on the line.
///
/// Vim looks for the number at or after the cursor rather than only under it, so
/// `3<C-a>` works wherever the cursor is on the line; a sighted minus belongs to
/// the number, and a number the cursor is in the middle of starts at its first
/// digit. Only decimal digits count: vim reads a leading `0` as octal and `0x` as
/// hexadecimal, and this does not, which is a difference worth knowing about
/// rather than a mistake.
fn number_at(buffer: &Buffer, at: usize) -> Option<(usize, usize)> {
    let (row, _) = buffer.char_to_line_col(at);
    let line_start = buffer.line_col_to_char(row, 0);
    let line_end = line_start + buffer.line_len(row);

    // A number the cursor is inside of started before it: walking back over the
    // digits takes the whole of `100` rather than the `0` under the cursor.
    let digit = {
        let mut index = at;
        while index > line_start
            && buffer
                .char_at(index - 1)
                .is_some_and(|c| c.is_ascii_digit())
        {
            index -= 1;
        }
        (index < at).then_some(index)
    }
    // Otherwise the first digit from the cursor onwards, which is how `<C-a>`
    // works from anywhere else on a line.
    .or_else(|| {
        (at..line_end).find(|index| buffer.char_at(*index).is_some_and(|c| c.is_ascii_digit()))
    })?;
    // A minus in front of the first digit is the number's own: `-3` counts as
    // one number, which is what vim does with the same line.
    let start = if digit > line_start && buffer.char_at(digit - 1) == Some('-') {
        digit - 1
    } else {
        digit
    };
    let mut end = digit;
    while end < line_end && buffer.char_at(end).is_some_and(|c| c.is_ascii_digit()) {
        end += 1;
    }
    Some((start, end))
}

/// `<C-a>` and `<C-x>`: the number under or after the cursor, moved by the count.
fn bump_number(editor: &mut Editor, params: &[Param], counter: Counter) -> Result<()> {
    let at = editor.cursor.char_idx(&editor.buffer);
    let Some((start, end)) = number_at(&editor.buffer, at) else {
        editor.message = "no number here".to_string();
        return Ok(());
    };
    // The slice may have taken the minus with it, which `parse` reads as the
    // sign it is.
    let text = editor.buffer.slice_text(start, end);
    let Ok(value) = text.parse::<i64>() else {
        editor.message = format!("{text} is not a number to count with");
        return Ok(());
    };
    let step = count(params) as i64;
    let value = match counter {
        Counter::Add => value.saturating_add(step),
        Counter::Subtract => value.saturating_sub(step),
    };
    editor.replace_range(start, end, &value.to_string(), Some(start));
    Ok(())
}

fn increment(editor: &mut Editor, params: &[Param]) -> Result<()> {
    bump_number(editor, params, Counter::Add)
}

fn decrement(editor: &mut Editor, params: &[Param]) -> Result<()> {
    bump_number(editor, params, Counter::Subtract)
}

/// `~`: swap the case of the characters from the cursor, moving over what it
/// changed.
///
/// Vim toggles rather than upper- or lower-cases, so running over a word twice
/// puts it back; the cursor ends after the last character changed. A count stops
/// at the end of the line: this is not a motion, and a `~` that walked into the
/// next line would be one.
fn toggle_case(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let start = editor.cursor.char_idx(&editor.buffer);
    let row = editor.cursor.row;
    let line_end = editor.buffer.line_col_to_char(row, 0) + editor.buffer.line_len(row);
    let end = (start + count(params)).min(line_end);
    if start >= end {
        return Ok(());
    }
    editor.change_case(start, end, CaseChange::Toggle);
    editor.cursor.set_char_idx(&editor.buffer, end);
    Ok(())
}

/// `%`: the bracket under the cursor, and where its pair is.
///
/// A bracket the cursor is not on is looked for on the cursor's line, so `%` in
/// front of a `(` still matches it, and a bracket with no pair is said so rather
/// than guessed at.
fn match_bracket(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let at = editor.cursor.char_idx(&editor.buffer);
    let row = editor.cursor.row;
    let Some((from, pair)) = bracket_at(&editor.buffer, row, at) else {
        editor.message = "no bracket here".to_string();
        return Ok(());
    };
    let Some(target) = pair else {
        editor.message = format!("no match for {from:?}");
        return Ok(());
    };
    editor.cursor.set_char_idx(&editor.buffer, target);
    Ok(())
}

/// The bracket `%` would work on, and where its pair is.
///
/// The bracket under the cursor, or else the first one on the cursor's line, so
/// `%` in front of a `(` still matches it. `None` for the whole thing means
/// there is no bracket there; `Some((at, None))` means there is one and it has
/// no pair. The pointer borrows this for a double click, where the manual says
/// "a double click on a character that has a match selects until that match
/// (like using `v%`)".
pub(crate) fn bracket_at(buffer: &Buffer, row: usize, at: usize) -> Option<(usize, Option<usize>)> {
    const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
    let line_start = buffer.line_col_to_char(row, 0);
    let line_end = line_start + buffer.line_len(row);
    let here = buffer.char_at(at);
    let opener = PAIRS
        .iter()
        .find(|(open, close)| here == Some(*open) || here == Some(*close))
        .or_else(|| {
            PAIRS.iter().find(|(open, close)| {
                (at..line_end).any(
                    |index| matches!(buffer.char_at(index), Some(c) if c == *open || c == *close),
                )
            })
        });
    let &(open, close) = opener?;
    // The bracket actually being matched, then its pair, searched outwards from
    // there — the same search the text objects use.
    let (from, to) = match here {
        Some(c) if c == open => (at, matching_close(buffer, at, open, close)),
        Some(c) if c == close => (at, enclosing_open(buffer, at, open, close)),
        _ => {
            let found = (at..line_end)
                .find(|index| matches!(buffer.char_at(*index), Some(c) if c == open || c == close))
                .unwrap_or(at);
            match buffer.char_at(found) {
                Some(c) if c == open => (found, matching_close(buffer, found, open, close)),
                _ => (found, enclosing_open(buffer, found, open, close)),
            }
        }
    };
    Some((from, to))
}

/// Which kind of find a motion name is, if it is one.
pub(crate) fn find_of(name: &str) -> Option<Find> {
    Some(match name {
        "find_forward" => Find::Forward,
        "find_backward" => Find::Backward,
        "till_forward" => Find::Till,
        "till_backward" => Find::TillBackward,
        _ => return None,
    })
}

/// The letter a mark command was given, either as its argument or as the one
/// the key before it left waiting.
///
/// The bare `` `a `` hands the letter in as a parameter; the motion path — `d`a`
/// — runs the command with the motion's name and nothing else, so the letter is
/// read from where the key that took it put it, exactly as a find's character
/// is.
fn mark_letter(editor: &Editor, params: &[Param]) -> Option<char> {
    str_param(params)
        .and_then(|name| name.chars().next())
        .or_else(|| editor.last_mark.map(|(letter, _)| letter))
}

/// `m{letter}` — put a mark down where the cursor is.
fn set_mark(editor: &mut Editor, params: &[Param]) -> Result<()> {
    if let Some(letter) = mark_letter(editor, params) {
        let cursor = editor.cursor;
        editor.marks.set(letter, cursor);
    }
    Ok(())
}

/// `'{letter}` — go to the first non-blank character of the mark's line.
fn mark_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    goto_mark(editor, params, MarkJump::Line)
}

/// `` `{letter} `` — go to the character the mark was put on.
fn mark_exact(editor: &mut Editor, params: &[Param]) -> Result<()> {
    goto_mark(editor, params, MarkJump::Exact)
}

/// Where a mark takes the cursor, and what happens when there is no mark.
///
/// The position is clamped into the text as it is now: a mark is not carried by
/// later edits, so it can be asked for a line that is no longer there.
fn goto_mark(editor: &mut Editor, params: &[Param], jump: MarkJump) -> Result<()> {
    let Some(letter) = mark_letter(editor, params) else {
        return Ok(());
    };
    editor.record_jump();
    let Some(mark) = editor.marks.get(letter) else {
        let message = format!("E20: Mark not set: {letter}");
        editor.message = message.clone();
        return Err(CommandError::Failed(message));
    };
    let last = editor.buffer.line_count().saturating_sub(1);
    let row = mark.row.min(last);
    match jump {
        // Only `m` ever wants a `Set`, and `m` does not come through here.
        MarkJump::Set => return Ok(()),
        MarkJump::Exact => {
            let col = mark.col.min(editor.buffer.line_len(row).saturating_sub(1));
            editor.cursor = Cursor::new(row, col);
        }
        MarkJump::Line => {
            editor.cursor = Cursor::new(row, 0);
            editor.move_to_first_non_blank();
        }
    }
    editor.clamp_normal_cursor();
    Ok(())
}

/// `''` — go back to where the last jump started, at its first non-blank.
fn jump_back_line(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    jump_back(editor, MarkJump::Line)
}

/// `` `` `` — go back to the exact character the last jump started on.
fn jump_back_exact(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    jump_back(editor, MarkJump::Exact)
}

/// Go back to the last jump, and remember this place as the one to come back to,
/// so the two keys toggle between two places the way vim's do.
fn jump_back(editor: &mut Editor, jump: MarkJump) -> Result<()> {
    let Some(from) = editor.jump_from else {
        let message = "E20: Mark not set".to_string();
        editor.message = message.clone();
        return Err(CommandError::Failed(message));
    };
    editor.record_jump();
    if let Some(mark) = Some(from) {
        let last = editor.buffer.line_count().saturating_sub(1);
        let row = mark.row.min(last);
        let col = match jump {
            MarkJump::Exact => mark.col.min(editor.buffer.line_len(row).saturating_sub(1)),
            _ => {
                let len = editor.buffer.line_len(row);
                let line_start = editor.buffer.line_col_to_char(row, 0);
                (0..len)
                    .find(|index| {
                        editor
                            .buffer
                            .char_at(line_start + index)
                            .is_some_and(|c| !c.is_whitespace())
                    })
                    .unwrap_or(0)
            }
        };
        editor.cursor = Cursor::new(row, col);
        editor.clamp_normal_cursor();
    }
    Ok(())
}

/// `g;` — back to the change before this one.
fn change_older(editor: &mut Editor, params: &[Param]) -> Result<()> {
    editor.record_jump();
    for _ in 0..count(params) {
        let Some(place) = editor.changes.back() else {
            editor.message = "E20: no older change".to_string();
            return Ok(());
        };
        editor.cursor = place;
        editor.clamp_normal_cursor();
    }
    Ok(())
}

/// `g,` — forward again, to the changes `g;` walked back over.
fn change_newer(editor: &mut Editor, params: &[Param]) -> Result<()> {
    editor.record_jump();
    for _ in 0..count(params) {
        let Some(place) = editor.changes.forward() else {
            editor.message = "E20: no newer change".to_string();
            return Ok(());
        };
        editor.cursor = place;
        editor.clamp_normal_cursor();
    }
    Ok(())
}

/// `:changes` — every place an edit was made, newest first.
fn list_changes(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if editor.changes.is_empty() {
        editor.message = "No changes".to_string();
        return Ok(());
    }
    let position = editor.changes.position();
    let places: Vec<String> = editor
        .changes
        .iter()
        .map(|(index, cursor)| {
            let here = if index + 1 == position { ">" } else { " " };
            let text = editor
                .buffer
                .line_string(cursor.row.min(editor.buffer.line_count().saturating_sub(1)));
            format!("{here}{} {} {}", index + 1, cursor.row + 1, text.trim())
        })
        .collect();
    editor.message = format!(
        "{} changes   {}",
        places.len(),
        places
            .into_iter()
            .rev()
            .take(6)
            .collect::<Vec<String>>()
            .join("   ")
    );
    Ok(())
}

/// `:delmarks {letters}` — take those marks away.
///
/// The letters are written together or apart (`:delmarks a b`, `:delmarks ab`)
/// and a hyphen names a run of them (`:delmarks a-c`), which is how vim reads
/// the argument too. A letter that is not one of the twenty-six is named back,
/// rather than taken for a mark that was never there.
fn delete_marks(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let Some(argument) = str_param(params) else {
        let message = "E471: Argument required".to_string();
        editor.message = message.clone();
        return Err(CommandError::Failed(message));
    };
    let mut letters = Vec::new();
    let mut chars = argument.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() || c == ',' {
            continue;
        }
        if !c.is_ascii_lowercase() {
            let message = format!("E474: Invalid argument: {c}");
            editor.message = message.clone();
            return Err(CommandError::Failed(message));
        }
        // `a-c` is every letter from one to the other, both ends included.
        if chars.peek() == Some(&'-') {
            let mut after = chars.clone();
            after.next();
            if let Some(last) = after.next().filter(char::is_ascii_lowercase) {
                chars.next();
                chars.next();
                for letter in c..=last {
                    letters.push(letter);
                }
                continue;
            }
        }
        letters.push(c);
    }
    if letters.is_empty() {
        let message = "E471: Argument required".to_string();
        editor.message = message.clone();
        return Err(CommandError::Failed(message));
    }
    let count = letters.len();
    for letter in letters {
        editor.marks.clear(letter);
    }
    editor.message = format!("{count} mark{} deleted", if count == 1 { "" } else { "s" });
    Ok(())
}

/// `:delmarks!` — take every mark of this buffer away.
fn delete_all_marks(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let count = editor.marks.iter().count();
    editor.marks.clear_all();
    editor.message = format!("{count} mark{} deleted", if count == 1 { "" } else { "s" });
    Ok(())
}

/// `:clearjumps` — forget where this file has been looked at from.
fn clear_jumps(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let count = editor.jumps.iter().count();
    editor.jumps.clear();
    editor.jump_from = None;
    editor.message = format!("{count} jump{} cleared", if count == 1 { "" } else { "s" });
    Ok(())
}

/// `<C-o>` — back to where the last jump came from, and the one before that.
fn jump_oldest(editor: &mut Editor, params: &[Param]) -> Result<()> {
    // Where we are now goes into the list first, so that `<C-i>` has somewhere
    // to come back to — the place left is what the walk is made of.
    editor.record_jump();
    for _ in 0..count(params) {
        let Some(place) = editor.jumps.back() else {
            editor.message = "E20: Mark not set".to_string();
            return Ok(());
        };
        editor.cursor = place;
        editor.clamp_normal_cursor();
    }
    Ok(())
}

/// `<C-i>` — forward again, through the jumps `<C-o>` walked back over.
fn jump_newest(editor: &mut Editor, params: &[Param]) -> Result<()> {
    editor.record_jump();
    for _ in 0..count(params) {
        let Some(place) = editor.jumps.forward() else {
            editor.message = "E20: Mark not set".to_string();
            return Ok(());
        };
        editor.cursor = place;
        editor.clamp_normal_cursor();
    }
    Ok(())
}

/// `:jumps` — every place in the jump list, and which one the walk is on.
fn list_jumps(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if editor.jumps.is_empty() {
        editor.message = "No jumps".to_string();
        return Ok(());
    }
    let position = editor.jumps.position();
    let places: Vec<String> = editor
        .jumps
        .iter()
        .map(|(index, cursor)| {
            let here = if index + 1 == position { ">" } else { " " };
            format!("{here}{} {}:{}", index + 1, cursor.row + 1, cursor.col + 1)
        })
        .collect();
    editor.message = format!(
        "{} jumps   {}",
        places.len(),
        places
            .into_iter()
            .rev()
            .take(8)
            .collect::<Vec<String>>()
            .join("   ")
    );
    Ok(())
}

/// `:marks` — every mark that has been put down.
fn list_marks(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if editor.marks.is_empty() {
        editor.message = "No marks".to_string();
        return Ok(());
    }
    let marks: Vec<String> = editor
        .marks
        .iter()
        .map(|(letter, cursor)| format!("{letter} {}:{}", cursor.row + 1, cursor.col + 1))
        .collect();
    editor.message = format!("{} marks   {}", marks.len(), marks.join("   "));
    Ok(())
}

/// `f{char}` — to the next `{char}` on the line.
fn find_forward(editor: &mut Editor, params: &[Param]) -> Result<()> {
    find(editor, params, Find::Forward)
}

/// `F{char}` — back to the previous `{char}` on the line.
fn find_backward(editor: &mut Editor, params: &[Param]) -> Result<()> {
    find(editor, params, Find::Backward)
}

/// `t{char}` — to just before the next `{char}`.
fn till_forward(editor: &mut Editor, params: &[Param]) -> Result<()> {
    find(editor, params, Find::Till)
}

/// `T{char}` — back to just after the previous `{char}`.
fn till_backward(editor: &mut Editor, params: &[Param]) -> Result<()> {
    find(editor, params, Find::TillBackward)
}

/// `;` — the last find again, the same way.
fn find_repeat(editor: &mut Editor, params: &[Param]) -> Result<()> {
    find_again(editor, params, false)
}

/// `,` — the last find again, the other way.
fn find_reverse(editor: &mut Editor, params: &[Param]) -> Result<()> {
    find_again(editor, params, true)
}

fn find_again(editor: &mut Editor, params: &[Param], reverse: bool) -> Result<()> {
    if let Some((_, kind)) = editor.last_find {
        find_with(editor, kind, reverse, count(params));
    }
    // Nothing has been looked for yet: vim does nothing, so nothing happens.
    Ok(())
}

fn find(editor: &mut Editor, params: &[Param], kind: Find) -> Result<()> {
    // A find names its character. `.` repeating one brings none, and then the
    // find that was made last is the one meant — which is also what `;` does.
    if let Some(target) = motion_param(params).and_then(|name| name.chars().next()) {
        editor.last_find = Some((target, kind));
    }
    find_with(editor, kind, false, count(params));
    Ok(())
}

/// Move to where `kind` finds `target`, `reverse` turning the direction around
/// and `count` saying how many of them to pass.
///
/// A find never leaves the line it started on: that is vim's rule, and it is
/// what keeps `df,` from deleting the rest of the file when the comma is not
/// there.
fn find_with(editor: &mut Editor, kind: Find, reverse: bool, count: usize) {
    let Some((target, _)) = editor.last_find else {
        return;
    };
    let row = editor.cursor.row;
    let len = editor.buffer.line_len(row);
    let line_start = editor.buffer.line_col_to_char(row, 0);
    let forward = matches!(kind, Find::Forward | Find::Till) != reverse;
    let mut col = editor.cursor.col;

    for _ in 0..count {
        let next = if forward {
            let from = col + 1;
            (from..len).find(|index| editor.buffer.char_at(line_start + index) == Some(target))
        } else {
            (0..col)
                .rev()
                .find(|index| editor.buffer.char_at(line_start + index) == Some(target))
        };
        match next {
            Some(found) => col = found,
            // Nowhere to go: the cursor stays where it is, the way vim leaves it.
            None => return,
        }
    }

    editor.cursor.col = match kind {
        Find::Forward | Find::Backward => col,
        // `t` and `T` stop next to the character rather than on it. The clamp
        // that keeps a normal-mode cursor on a real character happens in
        // `after_edit`, which is where every move goes through it.
        Find::Till => col.saturating_sub(1),
        Find::TillBackward => col + 1,
    };
    editor.after_edit();
}

/// `zt` — put the cursor's line at the top of the window, without moving the
/// cursor. `zz` and `zb` differ only in where the line goes.
fn scroll_to_top(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    scroll_cursor_line(editor, 0)
}

/// `zz` — the cursor's line in the middle.
fn scroll_to_center(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    scroll_cursor_line(editor, 1)
}

/// `zb` — the cursor's line at the bottom.
fn scroll_to_bottom(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    scroll_cursor_line(editor, 2)
}

/// Where the cursor's line goes: `place` is 0 for the top of the window, 1 for
/// the middle and 2 for the bottom of what it has room to draw.
///
/// The scroll is set *after* the bookkeeping every other move goes through,
/// because that bookkeeping ends by keeping the view inside the document — which
/// is right when the view is following the cursor and wrong when a command has
/// just said where to put it. `zz` on the last line of a file leaves empty rows
/// below, the way vim does.
fn scroll_cursor_line(editor: &mut Editor, place: usize) -> Result<()> {
    let rows = editor.viewport.text_rows() as usize;
    let offset = match place {
        0 => 0,
        1 => rows / 2,
        _ => rows.saturating_sub(1),
    };
    editor.after_edit();
    let last = editor.buffer.line_count().saturating_sub(1);
    editor.viewport.top_line = (editor.cursor.row + 1).saturating_sub(offset + 1).min(last);
    editor.redraw = true;
    Ok(())
}

fn move_up(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_up(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn move_down(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_down(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn move_word_forward(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_word_forward(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn move_word_backward(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_word_backward(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn move_line_start(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.move_line_start(&editor.buffer);
    editor.after_edit();
    Ok(())
}

fn move_line_end(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.move_line_end(&editor.buffer);
    editor.after_edit();
    Ok(())
}

fn goto_first_line(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.goto_first_line(&editor.buffer);
    editor.after_edit();
    Ok(())
}

fn goto_last_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    // With a count, `G` is a line number and not a repeat: `5G` is the fifth
    // line, which is what everyone means and the only place in the editor where
    // a count is not a count.
    match params.first() {
        Some(Param::Int(line)) if *line > 0 => {
            let row = (*line as usize - 1).min(editor.buffer.line_count().saturating_sub(1));
            editor.cursor.row = row;
            editor.cursor.col = 0;
        }
        _ => editor.cursor.goto_last_line(&editor.buffer),
    }
    editor.after_edit();
    Ok(())
}

/// How far a page scroll moves: one screen less a line of overlap, so the
/// reader keeps their bearings. A repeat count scrolls that many screens.
fn page_step(editor: &Editor, params: &[Param]) -> usize {
    let rows = editor.viewport.text_rows() as usize;
    rows.saturating_sub(1).max(1) * count(params)
}

/// Half a screen is a screen over two, and at least one line.
fn half_page_step(editor: &Editor, params: &[Param]) -> usize {
    (editor.viewport.text_rows() as usize / 2).max(1) * count(params)
}

/// `<C-e>` — scroll the view down a line, and take the cursor with it when the
/// screen reaches it.
///
/// Not a motion: vim does not let `d<C-e>` mean anything, and neither does this —
/// what it does is look, not move.
fn scroll_line_down(editor: &mut Editor, params: &[Param]) -> Result<()> {
    editor.scroll_view_by(count(params) as i64);
    editor.after_edit();
    Ok(())
}

/// `<C-y>` — the same, a line up.
fn scroll_line_up(editor: &mut Editor, params: &[Param]) -> Result<()> {
    editor.scroll_view_by(-(count(params) as i64));
    editor.after_edit();
    Ok(())
}

/// `<C-d>` — half a screen down: the cursor moves, and the view follows.
fn half_page_down(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..half_page_step(editor, params) {
        editor.cursor.move_down(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

/// `<C-u>` — the same, half a screen up.
fn half_page_up(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..half_page_step(editor, params) {
        editor.cursor.move_up(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

/// Which line of the *screen* a motion goes to.
#[derive(Clone, Copy)]
enum ScreenLine {
    /// The top row, or a count of rows below it.
    Top,
    /// The middle row.
    Middle,
    /// The bottom row, or a count of rows above it.
    Bottom,
}

/// `H` — the top line of the screen, or with a count that many below it.
///
/// These three are about the *screen* rather than the file, which is what makes
/// them different from `gg` and `G`: they say "the line I am looking at", and
/// they are linewise motions, so `dH` deletes up to what is on screen.
fn goto_screen_top(editor: &mut Editor, params: &[Param]) -> Result<()> {
    goto_screen_line(editor, ScreenLine::Top, count(params));
    Ok(())
}

/// `M` — the middle line of the screen.
fn goto_screen_middle(editor: &mut Editor, params: &[Param]) -> Result<()> {
    goto_screen_line(editor, ScreenLine::Middle, count(params));
    Ok(())
}

/// `L` — the bottom line of the screen, or with a count that many above it.
fn goto_screen_bottom(editor: &mut Editor, params: &[Param]) -> Result<()> {
    goto_screen_line(editor, ScreenLine::Bottom, count(params));
    Ok(())
}

fn goto_screen_line(editor: &mut Editor, place: ScreenLine, count: usize) {
    let rows = editor.viewport.text_rows() as usize;
    let top = editor.viewport.top_line;
    let row = match place {
        ScreenLine::Top => top + count.saturating_sub(1),
        ScreenLine::Middle => top + rows / 2,
        // The last row of the screen, and a count goes up from it — so `L` and
        // `3L` are the bottom row and the third from the bottom.
        ScreenLine::Bottom => (top + rows).saturating_sub(count.max(1)),
    };
    let last = editor.buffer.line_count().saturating_sub(1);
    editor.cursor.row = row.min(last);
    // The first character that is not blank, the way `^` goes: a screen motion
    // is about *which line*, not which column, and vim leaves the cursor at the
    // start of the text rather than in the indentation.
    editor.cursor.col = 0;
    editor.move_to_first_non_blank();
    editor.after_edit();
}

fn page_down(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..page_step(editor, params) {
        editor.cursor.move_down(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn page_up(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..page_step(editor, params) {
        editor.cursor.move_up(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn enter_insert(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.clear_pending();
    editor.mode = Mode::Insert;
    Ok(())
}

fn enter_insert_before(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.col = 0;
    editor.mode = Mode::Insert;
    editor.after_edit();
    Ok(())
}

fn append(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let end = editor.buffer.line_len(editor.cursor.row);
    editor.cursor.col = (editor.cursor.col + 1).min(end);
    editor.mode = Mode::Insert;
    editor.after_edit();
    Ok(())
}

fn append_eol(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.col = editor.buffer.line_len(editor.cursor.row);
    editor.mode = Mode::Insert;
    editor.after_edit();
    Ok(())
}

fn open_line_below(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    let indent = open_line_indent(editor, row);
    let terminator = open_line_terminator(editor, row);
    // Insert after the terminator that already ends this line: the new line
    // sits between the two, and index `at` is where its content starts.
    let at = editor
        .buffer
        .line_col_to_char(row, editor.buffer.line_len(row))
        + terminator.chars().count();
    editor.mode = Mode::Insert;
    // The new line is the indentation and then its own terminator, and the
    // cursor goes after the indentation — which is where typing starts.
    let text = format!("{indent}{terminator}");
    let caret = at + indent.chars().count();
    editor.replace_range(at, at, &text, Some(caret));
    Ok(())
}

/// The indentation a new line starts with: the last line's, when `autoindent`
/// says so, and nothing otherwise.
fn open_line_indent(editor: &Editor, row: usize) -> String {
    editor.indent_for_new_line(row)
}

fn open_line_above(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    let at = editor.buffer.line_col_to_char(row, 0);
    let indent = open_line_indent(editor, row);
    let terminator = open_line_terminator(editor, row);
    editor.clear_pending();
    editor.mode = Mode::Insert;
    // Index `at` now holds the indentation of the new line above, and its
    // terminator, and the cursor goes after the indentation.
    let text = format!("{indent}{terminator}");
    let caret = at + indent.chars().count();
    editor.replace_range(at, at, &text, Some(caret));
    Ok(())
}

/// The terminator a newly opened line borrows from its neighbour, so that a
/// CRLF file never sprouts a bare LF.
fn open_line_terminator(editor: &Editor, row: usize) -> String {
    let terminator = editor.buffer.line_terminator(row);
    if terminator.is_empty() {
        "\n".to_string()
    } else {
        terminator
    }
}

fn delete_char(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let Some((start, end)) = char_span(editor, count(params)) else {
        return Ok(());
    };
    editor.cut(start, end, Kind::Charwise, None);
    Ok(())
}

/// `<Del>` in insert mode: delete the character under the cursor *without*
/// taking it.
///
/// Vim's insert-mode delete does not touch the register, and neither does this
/// one: a delete while typing must not replace the text the user is about to
/// paste.
fn delete_forwards(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let Some((start, end)) = char_span(editor, count(params)) else {
        return Ok(());
    };
    editor.replace_range(start, end, "", None);
    Ok(())
}

/// The range `x` takes out: `count` characters from the cursor, or the
/// terminator of an empty line, which joins it with the line below.
///
/// One range rather than a loop of single deletions, so that the register holds
/// what the user asked for and a count stops at the end of the line.
fn char_span(editor: &Editor, count: usize) -> Option<(usize, usize)> {
    let row = editor.cursor.row;
    let line_len = editor.buffer.line_len(row);
    if editor.cursor.col >= line_len {
        // On the terminator (only possible on an empty line): all of it goes,
        // and the line below comes up. The last line has nothing to join to.
        if row + 1 >= editor.buffer.line_count() {
            return None;
        }
        let start = editor.buffer.line_col_to_char(row, line_len);
        let terminator = editor.buffer.line_terminator_len(row);
        return (terminator > 0).then_some((start, start + terminator));
    }
    let start = editor.cursor.char_idx(&editor.buffer);
    let end = start
        .saturating_add(count)
        .min(editor.buffer.line_col_to_char(row, line_len));
    (end > start).then_some((start, end))
}

/// `:p` — say what the current line is.
///
/// There is one line of message row and no scrollback, so this is what printing
/// a line can be here. It is also what `:g` and `:v` run when no command was
/// given: those collect their lines into that same row instead of calling this
/// once per line, which would leave only the last one showing.
fn print_line(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    editor.message = editor.buffer.line_string(row).trim_end().to_string();
    Ok(())
}

fn delete_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let (start, end) = line_span(editor, editor.cursor.row, count(params));
    editor.cut(start, end, Kind::Linewise, None);
    Ok(())
}

/// `yy`: whole lines into the register, and the text left alone.
fn yank_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let (start, end) = line_span(editor, editor.cursor.row, count(params));
    editor.copy(start, end, Kind::Linewise);
    Ok(())
}

/// `cc`: whole lines into the register, and insert where they were.
fn change_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let (start, end) = line_span(editor, editor.cursor.row, count(params));
    let span = Span {
        start,
        end,
        linewise: true,
    };
    let end = change_end(editor, &span);
    editor.cut(start, end, Kind::Linewise, Some(start));
    editor.mode = Mode::Insert;
    Ok(())
}

/// Where a linewise *change* ends: one terminator short of the delete, so the
/// line stays behind — empty — for the text about to be typed.
///
/// Vim's `cc` is not `dd` followed by `i`: it leaves an empty line where the text
/// was, and `cj` leaves one line where it took two.
fn change_end(editor: &Editor, span: &Span) -> usize {
    if !span.linewise || span.end <= span.start {
        return span.end;
    }
    let last_row = editor.buffer.char_to_line_col(span.end - 1).0;
    span.end
        .saturating_sub(editor.buffer.line_terminator_len(last_row))
}

/// The character range of `count` lines, from `row`, terminators included.
///
/// The start is pulled up when the count runs past the end of the buffer, which
/// is what makes `2dd` on the last line take it and the line above rather than
/// one line and nothing.
pub(crate) fn line_span(editor: &Editor, row: usize, count: usize) -> (usize, usize) {
    let lines = editor.buffer.line_count();
    let count = count.clamp(1, lines);
    let first = row.min(lines - count);
    let start = editor.buffer.line_col_to_char(first, 0);
    // Content plus its own terminator, line by line: enough to drop them
    // whatever the terminators are, without disturbing the line before.
    let end = start
        + (first..first + count)
            .map(|i| editor.buffer.line_len(i) + editor.buffer.line_terminator_len(i))
            .sum::<usize>();
    (start, end)
}

fn undo(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if !editor.undo() {
        editor.message = "Already at oldest change".to_string();
    }
    Ok(())
}

fn redo(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if !editor.redo() {
        editor.message = "Already at newest change".to_string();
    }
    Ok(())
}

fn enter_command(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.mode = Mode::Command;
    editor.prompt = Prompt::Command;
    editor.cmdline.clear();
    editor.clear_pending();
    editor.message.clear();
    Ok(())
}

/// `:s/pattern/replacement/[g]` — replace what the pattern finds, on this line
/// or (`%`) in the whole file.
///
/// The whole thing is one change as far as `u` is concerned, however many lines
/// it touched: a substitute is one command, and undoing half of one is not
/// something a person means to do.
fn substitute(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let Some(line) = motion_param(params) else {
        let msg = "E471: Argument required".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    };
    let spec = match Substitute::parse(line) {
        Ok(spec) => spec,
        Err(error) => {
            let msg = error.to_string();
            editor.message = msg.clone();
            return Err(CommandError::Failed(msg));
        }
    };
    // What `:&` will repeat.
    editor.last_substitute = Some(spec.clone());
    apply_substitute(editor, &spec)
}

/// Run a parsed substitute and say what it did.
fn apply_substitute(editor: &mut Editor, spec: &Substitute) -> Result<()> {
    // The `c` flag asks about every match, which is a conversation: the command
    // starts it and the answers finish it.
    if spec.confirm {
        return match editor.start_confirm(spec) {
            Ok(()) => Ok(()),
            Err(error) => {
                let msg = format!("E54: {error}");
                editor.message = msg.clone();
                Err(CommandError::Failed(msg))
            }
        };
    }
    let (count, rows) = match editor.substitute(spec) {
        Ok(result) => result,
        Err(error) => {
            let msg = format!("E54: {error}");
            editor.message = msg.clone();
            return Err(CommandError::Failed(msg));
        }
    };
    if count == 0 {
        let msg = format!("E486: Pattern not found: {}", spec.pattern);
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    }
    editor.message = format!(
        "{count} substitution{} on {rows} line{}",
        if count == 1 { "" } else { "s" },
        if rows == 1 { "" } else { "s" }
    );
    Ok(())
}

/// `/` — search forwards, typed on the same line a `:` command is.
fn search_forward(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    enter_prompt(editor, Prompt::SearchForward);
    Ok(())
}

/// `?` — search backwards.
fn search_backward(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    enter_prompt(editor, Prompt::SearchBackward);
    Ok(())
}

/// `n` — the next match, the same way round as the search that is up.
fn search_next(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let (row, col) = (editor.cursor.row, editor.cursor.col);
    repeat_search(editor, count(params), false);
    if (row, col) != (editor.cursor.row, editor.cursor.col) {
        // Where it *was*: the jump is remembered as the place it started, so
        // `''` comes back to the match before this one.
        editor.record_jump();
    }
    Ok(())
}

/// `N` — the next match the other way round.
fn search_previous(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let (row, col) = (editor.cursor.row, editor.cursor.col);
    repeat_search(editor, count(params), true);
    if (row, col) != (editor.cursor.row, editor.cursor.col) {
        editor.record_jump();
    }
    Ok(())
}

/// `:cnext` — the next place the last question named.
fn place_next(editor: &mut Editor, params: &[Param]) -> Result<()> {
    editor.walk_places(true, count(params));
    Ok(())
}

/// `:cprev` — the one before it.
fn place_previous(editor: &mut Editor, params: &[Param]) -> Result<()> {
    editor.walk_places(false, count(params));
    Ok(())
}

/// `gv` — the selection the last visual mode had.
fn reselect(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if !editor.reselect() {
        editor.message = "E20: No previous visual selection".to_string();
    }
    Ok(())
}

/// `gi` — where insert mode was last left, inserting there.
fn insert_again(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if !editor.insert_again() {
        editor.message = "E20: No previous insert position".to_string();
    }
    Ok(())
}

/// `:copen` — the list in a window of its own.
fn open_places(editor: &mut Editor, params: &[Param]) -> Result<()> {
    editor.open_places_list_where(str_param(params));
    Ok(())
}

/// `:colder` — the list before this one.
fn list_older(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.walk_list_history(-1);
    Ok(())
}

/// `:cnewer` — the list after it.
fn list_newer(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.walk_list_history(1);
    Ok(())
}

/// `:cclose` — close that window.
fn close_places(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.close_places_list();
    Ok(())
}

/// `:lopen` — what the language server said, in a window of its own.
fn open_diagnostics(editor: &mut Editor, params: &[Param]) -> Result<()> {
    editor.open_diagnostics_list_where(str_param(params));
    Ok(())
}

/// `:lclose` — close that window.
fn close_diagnostics(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.close_places_list();
    Ok(())
}

/// `<CR>` — go to the place the cursor's line names, when what is on screen is a
/// list of them.
fn open_place(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.open_place_at_cursor();
    Ok(())
}

/// `:clist` — what is in the list, and which one the walk is on.
fn place_list(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.message = editor.places_text();
    Ok(())
}

/// `<C-k>` and `:signature` — ask what the call being typed takes.
fn signature_help(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    let col = editor.cursor.col;
    editor.ask_server(Ask::Signature { row, col });
    Ok(())
}

/// `:format` — ask a language server to format this file.
fn format_document(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    let col = editor.cursor.col;
    editor.ask_server(Ask::Format {
        row,
        col,
        from: None,
        to: None,
        save: false,
    });
    Ok(())
}

/// `:{range}format` — the same, for the lines a range names.
///
/// The range arrives as two parameters, which is what the `:` line knows and what
/// the kernel's own command table passes.
fn format_range_command(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let from = match params.first() {
        Some(Param::Int(n)) => (*n).max(0) as usize,
        _ => 0,
    };
    let to = match params.get(1) {
        Some(Param::Int(n)) => (*n).max(0) as usize,
        _ => from,
    };
    let row = editor.cursor.row;
    let col = editor.cursor.col;
    editor.ask_server(Ask::Format {
        row,
        col,
        from: Some(from),
        to: Some(to),
        save: false,
    });
    Ok(())
}

/// `:wsymbols` — ask what the whole project holds.
fn workspace_symbols(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    let col = editor.cursor.col;
    let query = str_param(params).map(str::to_string);
    editor.ask_server(Ask::WorkspaceSymbols { row, col, query });
    Ok(())
}

/// `:symbols` — ask what this file holds.
fn list_symbols(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    let col = editor.cursor.col;
    editor.ask_server(Ask::Symbols { row, col });
    Ok(())
}

/// `gr` — ask where this name is used.
fn goto_references(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    let col = editor.cursor.col;
    editor.ask_server(Ask::References { row, col });
    Ok(())
}

fn enter_prompt(editor: &mut Editor, prompt: Prompt) {
    editor.mode = Mode::Command;
    editor.prompt = prompt;
    editor.cmdline.clear();
    editor.clear_pending();
    editor.message.clear();
    // A preview belongs to the line that was being typed; this is a new one.
    editor.forget_preview();
}

/// Go to the next match `count` times, wrapping round the document.
fn repeat_search(editor: &mut Editor, count: usize, reversed: bool) {
    if editor.search.is_empty() {
        editor.message = "E35: No previous search pattern".to_string();
        return;
    }
    let direction = if reversed {
        editor.search.direction().reversed()
    } else {
        editor.search.direction()
    };
    let mut at = editor.cursor.char_idx(&editor.buffer);
    let mut found = None;
    for _ in 0..count {
        match editor.search.next_from(at, direction) {
            Some((start, _)) => {
                found = Some(start);
                at = start;
            }
            None => break,
        }
    }
    match found {
        Some(start) => {
            editor.cursor.set_char_idx(&editor.buffer, start);
            editor.after_edit();
        }
        None => {
            editor.message = format!("E486: Pattern not found: {}", editor.search.pattern());
        }
    }
}

fn escape(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    // Leaving insert mode puts the completion list away and keeps what it has
    // typed: `<Esc>` is how a person says "this one, and stop offering".
    editor.accept_completion();
    let was_insert = editor.mode == Mode::Insert;
    editor.clear_pending();
    editor.mode = Mode::Normal;
    if was_insert {
        editor.cursor.move_left(&editor.buffer);
        editor.after_edit();
    }
    Ok(())
}

fn save(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let asked_for_layout = path_param(params).is_none() && editor.options.borrow().formatonsave;
    match editor.write_buffer(path_param(params)) {
        Ok(path) => {
            editor.message = format!("\"{}\" written", path.display());
            // `formatonsave` asks *after* the write, not instead of it: a server
            // that never answers must not cost a person their save. What it
            // answers is applied and written again, so the file on disk is what
            // the server laid out and the buffer is clean.
            if asked_for_layout {
                let row = editor.cursor.row;
                let col = editor.cursor.col;
                editor.ask_server(Ask::Format {
                    row,
                    col,
                    from: None,
                    to: None,
                    save: true,
                });
            }
            Ok(())
        }
        Err(e) => {
            editor.message = e.to_string();
            Err(CommandError::Failed(e.to_string()))
        }
    }
}

/// `:w!` — write even though the file changed on disk.
fn save_force(editor: &mut Editor, params: &[Param]) -> Result<()> {
    match editor.buffer.save_with(path_param(params), true) {
        Ok(path) => {
            // Forced writes go through the same marker as any other: what is on
            // disk is what is being looked at, however it got there.
            editor.history.mark_saved();
            editor.message = format!("\"{}\" written", path.display());
            Ok(())
        }
        Err(e) => {
            editor.message = e.to_string();
            Err(CommandError::Failed(e.to_string()))
        }
    }
}

fn quit(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    // With more than one window, `:q` closes this one; the last one quits the
    // session.
    if editor.windows.len() > 1 {
        if editor.is_dirty() {
            let msg = "E37: No write since last change (add ! to override)".to_string();
            editor.message = msg.clone();
            return Err(CommandError::Failed(msg));
        }
        editor.close_window(editor.current_window);
        return Ok(());
    }
    if editor.is_dirty() {
        let msg = "E37: No write since last change (add ! to override)".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    }
    editor.should_quit = true;
    Ok(())
}

fn quit_force(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.should_quit = true;
    Ok(())
}

fn save_quit(editor: &mut Editor, params: &[Param]) -> Result<()> {
    save(editor, params)?;
    editor.should_quit = true;
    Ok(())
}

fn edit_file(editor: &mut Editor, params: &[Param]) -> Result<()> {
    if editor.is_dirty() {
        let msg = "E37: No write since last change (add ! to override)".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    }
    open_file(editor, params)
}

/// `:e!` — the same, abandoning unsaved changes on purpose.
fn edit_file_force(editor: &mut Editor, params: &[Param]) -> Result<()> {
    open_file(editor, params)
}

fn reload_file(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let path = match path_param(params) {
        Some(p) => p.to_path_buf(),
        None => match editor.buffer.path().map(|p| p.to_path_buf()) {
            Some(p) => p,
            None => {
                let msg = "E32: No file name".to_string();
                editor.message = msg.clone();
                return Err(CommandError::Failed(msg));
            }
        },
    };
    match editor.load_file(&path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            editor.message = msg.clone();
            Err(CommandError::Failed(msg))
        }
    }
}

/// `:e [file]` — with no file, or the one that is already open, the document is
/// refilled from disk; a different file arrives as a document of its own, so the
/// one that was open keeps its place in the list rather than being lost.
fn open_file(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let path = match path_param(params) {
        Some(p) => p.to_path_buf(),
        None => return reload_file(editor, params),
    };
    if editor
        .buffer
        .path()
        .is_some_and(|open| same_file(open, &path))
    {
        return reload_file(editor, params);
    }
    // An empty window with no name is what the editor starts as, and the first
    // file to arrive takes its place rather than becoming a second document.
    if editor.buffer.path().is_none() && editor.history.is_untouched() {
        return reload_file(editor, params);
    }
    match editor.open_file(&path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            editor.message = msg.clone();
            Err(CommandError::Failed(msg))
        }
    }
}

/// Whether two paths name the same file. Both are asked what they really are
/// when they exist, so `:e ./a.txt` does not open a second copy of `a.txt`.
fn same_file(one: &Path, other: &Path) -> bool {
    if one == other {
        return true;
    }
    match (std::fs::canonicalize(one), std::fs::canonicalize(other)) {
        (Ok(one), Ok(other)) => one == other,
        _ => false,
    }
}

/// `:ls` — one line naming every open document, the one on screen first marked
/// the way vim marks it.
fn list_buffers(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let current = editor.current_document();
    let mut parts = Vec::with_capacity(editor.documents.len());
    for (index, document) in editor.documents.iter().enumerate() {
        // The line a document is "at" is where it was last looked at — which
        // for the one on screen is where the cursor is now.
        let row = if index == current {
            editor.cursor.row
        } else {
            document.cursor.row
        };
        // The document on screen is live in the editor's own fields, so its
        // slot holds a placeholder: ask the editor about that one.
        let name = if index == current {
            editor.buffer.display_name()
        } else {
            document.buffer.display_name()
        };
        let flag = if index == current { "%a" } else { " h" };
        parts.push(format!(
            "{} {} \"{}\" line {}",
            index + 1,
            flag,
            name,
            row + 1
        ));
    }
    editor.message = parts.join("   ");
    Ok(())
}

/// `:b <number>` or `:b <name>` — go to a document by number, or by a name that
/// is its path or its end.
fn buffer_go(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let Some(argument) = str_param(params) else {
        let msg = "E471: Argument required".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    };
    let found = match argument.parse::<usize>() {
        // A number is a position in the list, counted from one, the way `:ls`
        // numbers them.
        Ok(number) => (1..=editor.documents.len())
            .contains(&number)
            .then(|| number - 1),
        Err(_) => editor.documents.iter().position(|document| {
            // A path, or any part of one: `:b two` finds `dir/two.txt`, the
            // way vim's buffer matching does.
            document.buffer.display_name().contains(argument)
                || document
                    .buffer
                    .path()
                    .is_some_and(|path| path.to_string_lossy().contains(argument))
        }),
    };
    let Some(index) = found else {
        let msg = format!("E94: No matching buffer for {argument}");
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    };
    if index != editor.current_document() {
        editor.show_document(index);
        let name = editor.buffer.display_name();
        editor.message = format!("\"{name}\"");
    }
    Ok(())
}

/// `:bn` / `:bp` — the next or previous document, wrapping around the ends so a
/// pair of documents is all anyone needs.
fn buffer_next(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    step_buffer(editor, 1)
}

fn buffer_previous(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    step_buffer(editor, -1)
}

/// `:bn!` / `:bp!` — the same, abandoning the changes in the one being left.
fn buffer_next_force(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    step_buffer(editor, 1)
}

fn buffer_previous_force(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    step_buffer(editor, -1)
}

fn step_buffer(editor: &mut Editor, step: isize) -> Result<()> {
    let count = editor.documents.len();
    if count < 2 {
        let msg = "E85: There is no other buffer".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    }
    let from = editor.current_document() as isize;
    let index = (from + step).rem_euclid(count as isize) as usize;
    if editor.show_document(index) {
        let name = editor.buffer.display_name();
        editor.message = format!("\"{name}\"");
    }
    Ok(())
}

/// `:bd` — close the document that is on screen. A changed document refuses,
/// since closing it would throw the changes away.
fn buffer_delete(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if editor.is_dirty() {
        let msg = "E37: No write since last change (add ! to override)".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    }
    close_buffer(editor)
}

/// `:bd!` — close it and lose the changes, on purpose.
fn buffer_delete_force(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    close_buffer(editor)
}

fn close_buffer(editor: &mut Editor) -> Result<()> {
    let name = editor.buffer.display_name();
    let index = editor.current_document();
    editor.close_document(index);
    editor.message = format!("\"{name}\" deleted");
    Ok(())
}

/// `:sp [file]` — one window above another.
fn split_rows(editor: &mut Editor, params: &[Param]) -> Result<()> {
    split(editor, Axis::Rows, params)
}

/// `:vs [file]` — two windows side by side.
fn split_columns(editor: &mut Editor, params: &[Param]) -> Result<()> {
    split(editor, Axis::Columns, params)
}

fn split(editor: &mut Editor, axis: Axis, params: &[Param]) -> Result<()> {
    match path_param(params) {
        // A named file joins the list first — a path that is not there leaves
        // the screen as it was, and the window that asked for the split keeps
        // what it had — and the new window shows it.
        Some(path) => match editor.add_file(path) {
            Ok(document) => {
                editor.split_window(axis, Some(document));
            }
            Err(e) => {
                let msg = e.to_string();
                editor.message = msg.clone();
                return Err(CommandError::Failed(msg));
            }
        },
        None => {
            editor.split_window(axis, None);
        }
    }
    Ok(())
}

/// `<C-w>w` — the next window, wrapping around.
fn window_next(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let count = editor.windows.len();
    if count < 2 {
        let msg = "E85: There is no other window".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    }
    let next = (editor.current_window + 1) % count;
    editor.switch_to_window(next);
    Ok(())
}

/// `<C-w>h` `j` `k` `l` — the window in that direction.
fn window_left(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    step_window(editor, Direction::Left)
}

fn window_down(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    step_window(editor, Direction::Down)
}

fn window_up(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    step_window(editor, Direction::Up)
}

fn window_right(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    step_window(editor, Direction::Right)
}

/// Which way a window command looks for another window.
#[derive(Clone, Copy)]
enum Direction {
    Left,
    Down,
    Up,
    Right,
}

fn step_window(editor: &mut Editor, direction: Direction) -> Result<()> {
    if editor.windows.len() < 2 {
        let msg = "E85: There is no other window".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    }
    let rects = editor.window_rects();
    let Some(from) = rects.get(editor.current_window).copied() else {
        return Ok(());
    };
    // The rectangles are the honest answer: "down" is the nearest window whose
    // rows are below this one and whose columns overlap it.
    let mut best: Option<(usize, i32, u16)> = None;
    for (index, rect) in rects.iter().enumerate() {
        if index == editor.current_window {
            continue;
        }
        if !overlaps(from, *rect, direction) {
            continue;
        }
        let distance = match direction {
            Direction::Left => from.x as i32 - (rect.x + rect.width) as i32,
            Direction::Right => rect.x as i32 - (from.x + from.width) as i32,
            Direction::Up => from.y as i32 - (rect.y + rect.height) as i32,
            Direction::Down => rect.y as i32 - (from.y + from.height) as i32,
        };
        let along = match direction {
            Direction::Left | Direction::Right => rect.y,
            Direction::Up | Direction::Down => rect.x,
        };
        let nearer = |best: (usize, i32, u16)| {
            let (_, best_distance, best_along) = best;
            distance < best_distance || (distance == best_distance && along < best_along)
        };
        let better = match best {
            None => true,
            Some(best) => nearer(best),
        };
        if better {
            best = Some((index, distance, along));
        }
    }
    match best {
        Some((index, _, _)) => {
            editor.switch_to_window(index);
        }
        None => {
            let msg = "E16: Invalid range".to_string();
            editor.message = msg.clone();
            return Err(CommandError::Failed(msg));
        }
    }
    Ok(())
}

/// Whether the two rectangles are neighbours in `direction`: the target is on
/// that side, and the other axis overlaps at all.
fn overlaps(from: Rect, to: Rect, direction: Direction) -> bool {
    let sideways = |a: Rect, b: Rect| a.x < b.x + b.width && b.x < a.x + a.width;
    let stacked = |a: Rect, b: Rect| a.y < b.y + b.height && b.y < a.y + a.height;
    match direction {
        Direction::Left => to.x + to.width <= from.x && stacked(from, to),
        Direction::Right => to.x >= from.x + from.width && stacked(from, to),
        Direction::Up => to.y + to.height <= from.y && sideways(from, to),
        Direction::Down => to.y >= from.y + from.height && sideways(from, to),
    }
}

/// `:reg` — what every register holds.
fn list_registers(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let mut parts = vec!["--- Registers ---".to_string()];
    let show = |name: char, register: &crate::register::Register| {
        let text = register
            .text()
            .replace('\n', "^J")
            .replace('\r', "^M")
            .replace('\t', "^I");
        let kind = if register.is_linewise() {
            " linewise"
        } else {
            ""
        };
        format!("\"{name}   {text}{kind}")
    };
    if !editor.register.is_empty() {
        parts.push(show('"', &editor.register));
    }
    // The numbered ones, `"0` first, then the ones the user filled.
    let numbered: Vec<(char, &Register)> = ('0'..='9')
        .filter_map(|name| editor.registers.get(name).map(|register| (name, register)))
        .filter(|(_, register)| !register.is_empty())
        .collect();
    for (name, register) in numbered {
        parts.push(show(name, register));
    }
    for (name, register) in editor.registers.named() {
        parts.push(show(name, register));
    }
    editor.message = parts.join("   ");
    Ok(())
}

/// `<C-w>c` and `:q` with more than one window: close this one.
fn window_close(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.close_window(editor.current_window);
    Ok(())
}

/// `:on` — close every window but this one.
fn window_only(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.close_other_windows();
    Ok(())
}

/// `:&` — the last substitute again, on the line the cursor is on.
///
/// Vim's `&` repeats the pattern and the replacement but *not* the values of the
/// flags: `:%s/a/b/g` then `:&` replaces the first `a` on this line, which is
/// what makes it useful for walking down a file.
fn substitute_again(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let Some(mut spec) = editor.last_substitute.clone() else {
        let msg = "E33: No previous substitute regular expression".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    };
    spec.all = false;
    // The range of the line that was typed is not repeated either: `&` is about
    // *this* line.
    spec.from = None;
    spec.to = None;
    apply_substitute(editor, &spec)
}

/// `:&&` — the last substitute again, with the flags it had.
///
/// `:&` keeps the pattern and the replacement but not the flags, so it replaces
/// the first match on the line; `:&&` keeps those too, which is what makes a
/// `:s/x/y/g` worth repeating. Neither repeats the range: both are about the
/// line the cursor is on now.
fn substitute_again_flags(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let Some(mut spec) = editor.last_substitute.clone() else {
        let msg = "E33: No previous substitute regular expression".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    };
    spec.from = None;
    spec.to = None;
    apply_substitute(editor, &spec)
}

/// `:tabnew [file]` — a tab of its own, on an empty file or on the one named.
fn tab_new(editor: &mut Editor, params: &[Param]) -> Result<()> {
    match path_param(params) {
        Some(path) => match editor.add_file(path) {
            Ok(document) => {
                editor.open_tab(Some(document));
                let name = editor.buffer.display_name();
                editor.message = format!("\"{name}\"");
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                editor.message = msg.clone();
                Err(CommandError::Failed(msg))
            }
        },
        None => {
            editor.open_tab(None);
            Ok(())
        }
    }
}

/// `:tab split` — the document that is on screen, in a tab of its own.
fn tab_split(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let document = editor.current_document();
    editor.open_tab(Some(document));
    Ok(())
}

/// `:tabclose` — close this tab.
fn tab_close(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if !editor.close_tab() {
        let msg = "E784: Cannot close last tab page".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    }
    Ok(())
}

/// `:tabnext` and `:tabprevious`.
fn tab_next(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.step_tab(1);
    Ok(())
}

fn tab_previous(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.step_tab(-1);
    Ok(())
}

/// `:tabs` — what every tab is showing.
fn list_tabs(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let current = editor.current_tab;
    let mut parts = vec![
        "--- Tabs ---".to_string(),
        format!("{} tabs", editor.tabs.len()),
    ];
    parts.push(format!("tab {} of {}", current + 1, editor.tabs.len()));
    editor.message = parts.join("   ");
    Ok(())
}

/// `:resize [N]` — make this window that many rows tall, or one taller.
fn resize(editor: &mut Editor, params: &[Param]) -> Result<()> {
    resize_axis(editor, Axis::Rows, params)
}

/// `:vertical resize [N]` — the same across.
fn resize_vertical(editor: &mut Editor, params: &[Param]) -> Result<()> {
    resize_axis(editor, Axis::Columns, params)
}

fn resize_axis(editor: &mut Editor, axis: Axis, params: &[Param]) -> Result<()> {
    // A bare `:resize` grows by one. `:resize 5` sets the size; `:resize -2`
    // and `:resize +2` move it, which is how a size is walked up and down.
    let size = match params.first() {
        Some(Param::Int(n)) if *n >= 0 => Resize::Set(*n as u16),
        Some(Param::Int(n)) => Resize::By(*n as i32),
        Some(Param::Str(text)) => {
            let text = text.trim();
            // A sign is what tells `:resize 5` from `:resize +5`: the first
            // asks for a size and the second for five more.
            let moves = text.starts_with('+') || text.starts_with('-');
            match text.parse::<i32>() {
                Ok(n) if moves => Resize::By(n),
                Ok(n) if n > 0 => Resize::Set(n.min(u16::MAX as i32) as u16),
                Ok(_) => Resize::One,
                Err(_) => {
                    let msg = format!("E475: Invalid argument: {text}");
                    editor.message = msg.clone();
                    return Err(CommandError::Failed(msg));
                }
            }
        }
        _ => Resize::One,
    };
    if !editor.resize_window(axis, size) {
        // A window with nothing beside it is the whole of its rectangle, and
        // there is nothing to move but the screen.
        editor.message = "E36: Not enough room".to_string();
    }
    Ok(())
}

/// `<C-w>=` — give every window its equal share again.
fn equalize_windows(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.equalize_windows();
    Ok(())
}

/// `K` — what is this? The answer comes from a language server when one is
/// running, and there is nothing to say when one is not.
fn hover(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.ask_server(Ask::Hover {
        row: editor.cursor.row,
        col: editor.cursor.col,
    });
    Ok(())
}

/// `gd` — where is this defined?
fn goto_definition(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.ask_server(Ask::Definition {
        row: editor.cursor.row,
        col: editor.cursor.col,
    });
    Ok(())
}

/// `<C-n>` — the next thing that could be typed, or the next item in the list.
fn complete_next(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.complete(Some(1));
    Ok(())
}

/// `<C-p>` — the one before it.
fn complete_previous(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.complete(Some(-1));
    Ok(())
}

/// `<C-y>` — type the one that is picked.
fn complete_accept(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.accept_completion();
    Ok(())
}

/// `<Tab>` in insert mode: the next place a snippet wants something, or a tab.
///
/// A snippet that has been typed from a completion list is a session, and this
/// is the key that walks it. With nothing to walk, the key is a tab — which is
/// what it is for the rest of the time.
fn snippet_next(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if !editor.snippet_step(true) {
        editor.type_text("\t");
    }
    Ok(())
}

/// `<S-Tab>` in insert mode: back to the place before this one, or a tab.
fn snippet_previous(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if !editor.snippet_step(false) {
        editor.type_text("\t");
    }
    Ok(())
}

/// `<C-n>` while a snippet is being filled in: the next thing this place allows.
///
/// The completion list has the key for the rest of the time; a snippet session
/// takes it only when there is no list up, which is when there is nothing else
/// for it to mean.
fn snippet_choice_next(editor: &mut Editor, params: &[Param]) -> Result<()> {
    if editor.completion.is_none() && editor.snippet_choose(true) {
        return Ok(());
    }
    complete_next(editor, params)
}

/// `<C-p>` in a snippet session: the choice before this one.
fn snippet_choice_previous(editor: &mut Editor, params: &[Param]) -> Result<()> {
    if editor.completion.is_none() && editor.snippet_choose(false) {
        return Ok(());
    }
    complete_previous(editor, params)
}

/// `<C-e>` — put the list away.
fn complete_cancel(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cancel_completion();
    Ok(())
}

/// `set_diagnostics <spec>` — hand the editor what a language server said.
///
/// The spec is one entry per diagnostic, separated by `;`, each written
/// `line:col:end-line:end-col:severity:message` with one-based line and column
/// numbers — the numbers a person sees, and the numbers a compiler's output
/// uses. A client that speaks a real protocol converts to rows and character
/// columns and calls the kernel directly; this is the seam a script uses.
fn set_diagnostics(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let Some(spec) = str_param(params) else {
        let msg = "E471: Argument required".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    };
    match parse_diagnostics(spec, editor.buffer.line_count()) {
        Ok(diagnostics) => {
            editor.set_diagnostics(diagnostics);
            Ok(())
        }
        Err(bad) => {
            let msg = format!("E475: Invalid diagnostic: {bad}");
            editor.message = msg.clone();
            Err(CommandError::Failed(msg))
        }
    }
}

/// Parse the spec `set_diagnostics` takes. The first bad entry comes back.
fn parse_diagnostics(
    spec: &str,
    lines: usize,
) -> Result<Vec<crate::diagnostic::Diagnostic>, String> {
    use crate::diagnostic::{Diagnostic, Severity};
    let mut out = Vec::new();
    for entry in spec.split(';').filter(|entry| !entry.trim().is_empty()) {
        let fields: Vec<&str> = entry.splitn(6, ':').collect();
        let [start_line, start_col, end_line, end_col, severity, message] = fields[..] else {
            return Err(entry.to_string());
        };
        let number = |text: &str| text.trim().parse::<usize>().ok();
        let (Some(start_line), Some(start_col)) = (number(start_line), number(start_col)) else {
            return Err(entry.to_string());
        };
        let (Some(end_line), Some(end_col)) = (number(end_line), number(end_col)) else {
            return Err(entry.to_string());
        };
        let severity = match severity.trim() {
            "error" | "E" => Severity::Error,
            "warning" | "W" => Severity::Warning,
            "info" | "I" => Severity::Information,
            "hint" | "H" => Severity::Hint,
            _ => return Err(entry.to_string()),
        };
        // One-based, clamped into the file: a server that is out of date says
        // so about a line that is no longer there.
        let row = start_line.saturating_sub(1).min(lines.saturating_sub(1));
        let end_row = end_line.saturating_sub(1).min(lines.saturating_sub(1));
        out.push(Diagnostic::new(
            (row, start_col.saturating_sub(1)),
            (end_row, end_col.saturating_sub(1)),
            severity,
            message.trim(),
        ));
    }
    Ok(out)
}

/// `]d` — the next diagnostic, wrapping around.
fn next_diagnostic(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.step_diagnostic(true);
    Ok(())
}

/// `[d` — the one before it.
fn previous_diagnostic(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.step_diagnostic(false);
    Ok(())
}

/// `:diagnostics` — what a language server has said about this document.
fn list_diagnostics(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if editor.diagnostics.is_empty() {
        editor.message = "No diagnostics".to_string();
        return Ok(());
    }
    let mut parts = vec![format!("{} diagnostics", editor.diagnostics.len())];
    for diagnostic in editor.diagnostics.iter().take(8) {
        parts.push(format!(
            "{}:{} {}",
            diagnostic.start.0 + 1,
            diagnostic.start.1 + 1,
            diagnostic.line()
        ));
    }
    if editor.diagnostics.len() > 8 {
        parts.push(format!("... {} more", editor.diagnostics.len() - 8));
    }
    editor.message = parts.join("   ");
    Ok(())
}

/// `.` — do the last change again.
///
/// A change is a command that changed the buffer, or one that opened an insert
/// session: the session's text is typed again after the command has run, so
/// `cwfoo<Esc>` then `.` changes the next word to `foo` and nothing has to
/// remember what "the word" was.
fn repeat(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let Some((command, args)) = editor.last_command.clone() else {
        // Nothing has been changed yet. Vim beeps and changes nothing.
        return Ok(());
    };
    let insert = editor.last_insert.clone();
    for _ in 0..count(params) {
        editor.run_command(&command, &args);
        if let Some(text) = insert.as_deref() {
            editor.replay_insert(text);
        }
    }
    Ok(())
}

/// `*` — search for the word under the cursor, forwards.
fn search_word_forward(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    search_word(editor, crate::search::Direction::Forward)
}

/// `#` — the same, backwards.
fn search_word_backward(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    search_word(editor, crate::search::Direction::Backward)
}

fn search_word(editor: &mut Editor, direction: crate::search::Direction) -> Result<()> {
    let Some(word) = word_at_cursor(editor) else {
        let msg = "E348: No string under cursor".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    };
    let pattern = word_pattern(&word);
    editor.run_search(&pattern, direction);
    // A search stays on a match the cursor is already in — `n` is what moves on
    // — and `*` is asking for the next one of these, so it moves on at once.
    if !editor.search.is_empty() {
        repeat_search(editor, 1, false);
    }
    Ok(())
}

/// The word the cursor is on, or the next one after it when the cursor is not
/// on a word at all.
///
/// Not a run of spaces: `*` on whitespace means "find this word again", and a
/// run of spaces is nobody's word.
fn word_at_cursor(editor: &Editor) -> Option<String> {
    let buffer = &editor.buffer;
    let mut at = editor.cursor.char_idx(buffer);
    while let Some(c) = buffer.char_at(at) {
        if word_class(c) == WordClass::Keyword {
            break;
        }
        at += 1;
    }
    let (start, end) = word_object(buffer, at, Object::Inner)?;
    (start < end).then(|| buffer.slice_text(start, end))
}

/// The pattern `*` and `#` search for: the word, with word boundaries where it
/// has word characters at its ends.
///
/// A word that is all punctuation — `*` on `(` — has no boundary to ask for, so
/// it searches for the characters themselves.
fn word_pattern(word: &str) -> String {
    let word_like = |c: char| c.is_alphanumeric() || c == '_';
    let mut pattern = String::new();
    if word.starts_with(word_like) {
        pattern.push_str("\\b");
    }
    pattern.push_str(&regex::escape(word));
    if word.ends_with(word_like) {
        pattern.push_str("\\b");
    }
    pattern
}

// ---------------------------------------------------------------------------
// Text objects
// ---------------------------------------------------------------------------

/// A linewise span for `row` and `count` lines.
fn lines_as_span(editor: &Editor, row: usize, count: usize) -> Option<Span> {
    let (start, end) = line_span(editor, row, count);
    (start < end).then_some(Span {
        start,
        end,
        linewise: true,
    })
}

/// The range a text object names.
///
/// `None` when there is nothing of that kind around the cursor. What is *around*
/// it is decided by scanning the buffer's characters and nothing else: a
/// parenthesis inside a string counts as a parenthesis, and a quote in a comment
/// as a quote. The highlighter knows better, and a day will come when this asks
/// it.
pub(crate) fn object_span(editor: &Editor, object: Object, key: &str) -> Option<Span> {
    let buffer = &editor.buffer;
    let at = editor.cursor.char_idx(buffer);
    let charwise = |span: (usize, usize)| {
        Some(Span {
            start: span.0,
            end: span.1,
            linewise: false,
        })
    };
    match key {
        "w" => charwise(word_object(buffer, at, object)?),
        "p" => paragraph_object(editor, at, object),
        "\"" | "'" | "`" => {
            let quote = key.chars().next()?;
            charwise(quote_object(buffer, at, quote, object)?)
        }
        "(" | ")" | "b" => charwise(pair_object(buffer, at, '(', ')', object)?),
        "[" | "]" => charwise(pair_object(buffer, at, '[', ']', object)?),
        "{" | "}" | "B" => charwise(pair_object(buffer, at, '{', '}', object)?),
        "<" | ">" => charwise(pair_object(buffer, at, '<', '>', object)?),
        _ => None,
    }
}

/// `ip` and `ap`: the lines of the paragraph the cursor is in.
///
/// A paragraph is a run of lines that are not blank. `ap` also takes the blank
/// lines after it — or the ones before it, when the paragraph ends the buffer,
/// which is the same rule `aw` follows for the space around a word.
fn paragraph_object(editor: &Editor, at: usize, object: Object) -> Option<Span> {
    let buffer = &editor.buffer;
    let blank = |row: usize| buffer.line_string(row).trim().is_empty();
    let last = buffer.line_count().saturating_sub(1);

    let mut row = buffer.char_to_line_col(at).0;
    if blank(row) && object == Object::Inner {
        // On a blank line `ip` is the blank lines themselves: the run of them
        // the cursor is in, which is what `dip` between two paragraphs means.
        let mut first = row;
        while first > 0 && blank(first - 1) {
            first -= 1;
        }
        let mut end = row;
        while end < last && blank(end + 1) {
            end += 1;
        }
        return lines_as_span(editor, first, end - first + 1);
    }
    while blank(row) && row < last {
        row += 1;
    }
    let mut first = row;
    while first > 0 && !blank(first - 1) {
        first -= 1;
    }
    let mut end = row;
    while end < last && !blank(end + 1) {
        end += 1;
    }
    if object == Object::Around {
        // The blank lines that follow, if there are any; otherwise those before.
        let mut after = end;
        while after < last && blank(after + 1) {
            after += 1;
        }
        if after > end {
            end = after;
        } else {
            while first > 0 && blank(first - 1) {
                first -= 1;
            }
        }
    }
    lines_as_span(editor, first, end - first + 1)
}

/// What kind of character a word is made of, for `iw` and `aw`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WordClass {
    /// Letters, digits and the underscore: what `w` moves over.
    Keyword,
    /// Everything else that is not a space, one run at a time.
    Punctuation,
    /// Spaces, tabs and the like.
    Space,
}

fn word_class(c: char) -> WordClass {
    if c.is_alphanumeric() || c == '_' {
        WordClass::Keyword
    } else if c.is_whitespace() {
        WordClass::Space
    } else {
        WordClass::Punctuation
    }
}

/// `iw` and `aw`.
///
/// The word is the run of one kind of character the cursor is in. On whitespace
/// that run *is* the whitespace, which is what vim does and what makes `diw`
/// between two words take the space rather than the next word.
pub(crate) fn word_object(buffer: &Buffer, at: usize, object: Object) -> Option<(usize, usize)> {
    let here = buffer.char_at(at)?;
    let class = word_class(here);
    if class == WordClass::Space && object == Object::Around {
        // `aw` on whitespace is that whitespace: there is nothing to grow into.
        let (start, end) = run_around(buffer, at, class);
        return Some((start, end));
    }
    let (start, end) = run_around(buffer, at, class);
    if object == Object::Inner {
        return Some((start, end));
    }

    // `aw` takes the whitespace after the word, or the whitespace before it when
    // the word ends the line — vim's rule, and the reason `daw` on the last word
    // of a line does not leave a trailing space behind.
    let mut after = end;
    while buffer.char_at(after).is_some_and(char::is_whitespace) {
        // Never across a line break: `daw` takes the space, not the line.
        if buffer.char_at(after) == Some('\n') {
            break;
        }
        after += 1;
    }
    if after > end {
        return Some((start, after));
    }
    let mut before = start;
    while before > 0
        && buffer.char_at(before - 1).is_some_and(char::is_whitespace)
        && buffer.char_at(before - 1) != Some('\n')
    {
        before -= 1;
    }
    Some((before, end))
}

/// The run of characters of `class` around character `at`.
fn run_around(buffer: &Buffer, at: usize, class: WordClass) -> (usize, usize) {
    let mut start = at;
    while start > 0
        && buffer
            .char_at(start - 1)
            .is_some_and(|c| word_class(c) == class)
    {
        start -= 1;
    }
    let mut end = at;
    while buffer.char_at(end).is_some_and(|c| word_class(c) == class) {
        end += 1;
    }
    (start, end)
}

/// `i"`, `a"` and their relatives: the quoted text on the line the cursor is on.
///
/// Vim does not leave the line for a quote, so neither does this: a string that
/// runs over a line break is found where it starts, and not from the line after.
fn quote_object(buffer: &Buffer, at: usize, quote: char, object: Object) -> Option<(usize, usize)> {
    let line = buffer.char_to_line_col(at).0;
    let line_start = buffer.line_col_to_char(line, 0);
    let line_len = buffer.line_len(line);
    let line_end = line_start + line_len;
    let quotes: Vec<usize> = (line_start..line_end)
        .filter(|index| buffer.char_at(*index) == Some(quote))
        .collect();

    // Pairs, in order. The cursor inside one takes it; otherwise the first pair
    // that starts at or after the cursor, which is how `ci"` before a string
    // changes the string ahead rather than nothing.
    let mut pairs = quotes.chunks_exact(2).map(|pair| (pair[0], pair[1]));
    let pair = pairs
        .clone()
        .find(|(open, close)| *open <= at && at <= *close)
        .or_else(|| pairs.find(|(open, _)| *open >= at))?;
    Some(match object {
        Object::Inner => (pair.0 + 1, pair.1),
        Object::Around => (pair.0, pair.1 + 1),
    })
}

/// `i(`, `a(`, `i{`, `a<` and their relatives: the innermost pair of brackets
/// around the cursor, or the first pair that starts after it.
///
/// The *matching* bracket is searched for across the whole buffer — a call with
/// arguments on four lines is one block, and that is the block people mean — but
/// a pair the cursor is not inside is only looked for on the cursor's line, so
/// `ci(` cannot travel to the end of the file by surprise.
fn pair_object(
    buffer: &Buffer,
    at: usize,
    open: char,
    close: char,
    object: Object,
) -> Option<(usize, usize)> {
    let outside = || {
        let line = buffer.char_to_line_col(at).0;
        let line_start = buffer.line_col_to_char(line, 0);
        let line_end = line_start + buffer.line_len(line);
        (line_start..line_end).find(|index| buffer.char_at(*index) == Some(open))
    };
    let opener = enclosing_open(buffer, at, open, close).or_else(outside)?;
    let closer = matching_close(buffer, opener, open, close)?;
    Some(match object {
        Object::Inner => (opener + 1, inner_end(buffer, opener + 1, closer)),
        Object::Around => (opener, closer + 1),
    })
}

/// Where the inside of a pair ends.
///
/// When the pair spans lines, the line break the closing bracket sits on belongs
/// to the *block* rather than to what is inside it: `di{` on a function leaves
/// the braces on lines of their own, which is what anyone typing it means.
fn inner_end(buffer: &Buffer, start: usize, closer: usize) -> usize {
    if closer > start && buffer.char_at(closer - 1) == Some('\n') {
        return closer - 1;
    }
    closer
}

/// The unmatched `open` before `at` that `close` characters have not been
/// paired off with.
fn enclosing_open(buffer: &Buffer, at: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = at.min(buffer.len_chars());
    while index > 0 {
        index -= 1;
        match buffer.char_at(index) {
            Some(c) if c == close => depth += 1,
            Some(c) if c == open => {
                if depth == 0 {
                    return Some(index);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// Where the pair opened at `opener` closes.
fn matching_close(buffer: &Buffer, opener: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = opener;
    while index < buffer.len_chars() {
        match buffer.char_at(index) {
            Some(c) if c == open => depth += 1,
            Some(c) if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Motions, operators and the register
// ---------------------------------------------------------------------------

/// What a motion's range covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MotionKind {
    /// Characters, from within a line.
    Charwise,
    /// Whole lines, terminators and all.
    Linewise,
}

/// A motion an operator can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Motion {
    /// What the range the motion makes covers.
    pub kind: MotionKind,
    /// Whether the character the motion lands on is part of the range. Vim's
    /// `$` is inclusive; `l`, `w` and `b` are not.
    pub(crate) inclusive: bool,
}

/// The motions an operator takes, by the name of the command that performs them.
///
/// A key that is not bound to one of these cannot follow `d`, `y` or `c`, which
/// is what keeps `d` followed by `i` from entering insert mode. Motions are named
/// by their commands, so a motion is reachable exactly where the keymap reaches
/// it — including a user's remapping of `w` to something else.
pub(crate) fn motion_of(name: &str) -> Option<Motion> {
    let charwise = |inclusive| Motion {
        kind: MotionKind::Charwise,
        inclusive,
    };
    let linewise = Motion {
        kind: MotionKind::Linewise,
        inclusive: true,
    };
    Some(match name {
        "move_left" | "move_line_start" | "move_word_backward" => charwise(false),
        "move_right" | "move_word_forward" => charwise(false),
        "move_line_end" => charwise(true),
        // All four finds are *inclusive*: each lands on the last character the
        // operator should take. For `f` that is the character found, and for
        // `t` it is the one before it — which is exactly what `dt)` should
        // delete, and why `dt)` leaves the bracket behind.
        "find_forward" | "find_backward" | "find_repeat" | "find_reverse" | "till_forward"
        | "till_backward" => charwise(true),
        "move_up" | "move_down" | "goto_first_line" | "goto_last_line" | "page_up"
        | "page_down" | "goto_screen_top" | "goto_screen_middle" | "goto_screen_bottom" => linewise,
        // A mark's line is a line and its character is a character, which is
        // the difference vim draws between `'a` and `` `a `` — `d'a` takes the
        // whole line, `d`a` everything up to the mark.
        "format_motion" | "reindent_motion" | "indent_motion" | "outdent_motion" | "mark_line"
        | "jump_back_line" => linewise,
        "mark_exact" | "jump_back_exact" => charwise(false),
        _ => return None,
    })
}

/// The motion a binding names, if it names exactly one.
pub(crate) fn binding_motion(binding: &Binding) -> Option<&str> {
    let mut commands = binding.commands.iter();
    let name = commands.next()?;
    if commands.next().is_some() {
        return None;
    }
    motion_of(name).map(|_| name.as_str())
}

/// A range an operator works on, as character indices.
pub(crate) struct Span {
    /// The first character of the range.
    pub(crate) start: usize,
    /// One past its last character.
    pub(crate) end: usize,
    /// Whether it covers whole lines, terminators and all.
    pub(crate) linewise: bool,
}

/// Where a motion takes the cursor, as a span from where it started.
///
/// The motion is run for real: `w`, `$` and their neighbours already know how to
/// find their target, and a second copy of that would be a second copy of the
/// line model. The cursor is put back where it was, so the operator decides
/// where it ends up.
fn motion_span(editor: &mut Editor, name: &str, count: usize) -> Option<Span> {
    let motion = motion_of(name)?;
    // `v`, `V` or `<C-v>` after the operator overrides what the motion *is* —
    // vim's `o_v`, `o_V` and `o_CTRL-V`. The shape is the operator's, not the
    // motion's, and it is spent here: one operator takes one motion.
    let forced = editor.forced_shape.take().map(|(shape, _)| shape);
    let before = editor.cursor;
    let before_idx = before.char_idx(&editor.buffer);
    let run = editor.registry.get(name)?;
    for _ in 0..count {
        run(editor, &[]).ok()?;
    }
    let after = editor.cursor;
    editor.cursor = before;
    editor.after_edit();

    // `V`: whole lines, whatever the motion was.
    let linewise = match forced {
        Some(ForcedShape::Linewise) => true,
        Some(ForcedShape::Charwise) => false,
        _ => motion.kind == MotionKind::Linewise,
    };
    if linewise {
        let (first, last) = ordered(before.row, after.row);
        let (start, end) = line_span(editor, first, last - first + 1);
        return Some(editor.widen_over_folds(Span {
            start,
            end,
            linewise: true,
        }));
    }

    // `v` on a linewise motion makes it exclusive, and on a characterwise one it
    // turns inclusive/exclusive the other way round (`o_v`).
    let inclusive = match forced {
        Some(ForcedShape::Charwise) => motion.kind != MotionKind::Linewise && !motion.inclusive,
        _ => motion.inclusive,
    };
    let after_idx = after.char_idx(&editor.buffer);
    let (start, end) = if after_idx >= before_idx {
        let end = if inclusive {
            // One column further, which is the whole grapheme: `$` on an empty
            // line lands on column zero and so takes nothing.
            editor.buffer.line_col_to_char(after.row, after.col + 1)
        } else {
            after_idx
        };
        (before_idx, end)
    } else {
        (after_idx, before_idx)
    };
    // A closed fold is a whole: an operator whose motion reaches into one takes all
    // of it. (The manual's example is `dl` deleting the whole fold under the cursor.)
    Some(editor.widen_over_folds(Span {
        start,
        end,
        linewise: false,
    }))
}

/// The end of the run of non-blank characters starting at `from`.
///
/// This is what makes `cw` vim's `cw`: it changes the word, not the space after
/// it, which is `ce` in disguise.
fn word_run_end(editor: &Editor, from: usize) -> usize {
    let mut end = from;
    while let Some(c) = editor.buffer.char_at(end) {
        if c.is_whitespace() {
            break;
        }
        end += 1;
    }
    end
}

fn ordered(a: usize, b: usize) -> (usize, usize) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// `d` — take a range out, or start waiting for the motion that makes it.
fn delete_motion(editor: &mut Editor, params: &[Param]) -> Result<()> {
    match motion_param(params) {
        None => {
            editor.operator = Some(Operator::Delete);
            editor.operator_count = count(params);
        }
        Some(name) => {
            let count = count(params);
            if let Some(span) = motion_span(editor, name, count) {
                let kind = span_kind(&span);
                // Vim leaves the cursor where the range started.
                editor.cut(span.start, span.end, kind, Some(span.start));
            }
        }
    }
    Ok(())
}

/// `y` — copy a range into the register, or start waiting for its motion.
fn yank_motion(editor: &mut Editor, params: &[Param]) -> Result<()> {
    match motion_param(params) {
        None => {
            editor.operator = Some(Operator::Yank);
            editor.operator_count = count(params);
        }
        Some(name) => {
            let count = count(params);
            if let Some(span) = motion_span(editor, name, count) {
                let kind = span_kind(&span);
                editor.copy(span.start, span.end, kind);
            }
        }
    }
    Ok(())
}

/// What `g~`, `gu` and `gU` do to the text they take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaseChange {
    /// Swap the case of every letter.
    Toggle,
    /// Make every letter lower case.
    Lower,
    /// Make every letter upper case.
    Upper,
}

/// The shape `g~`, `gu` and `gU` all have: a motion, or waiting for one.
fn case_motion(editor: &mut Editor, params: &[Param], change: CaseChange, operator: Operator) {
    match motion_param(params) {
        None => {
            editor.operator = Some(operator);
            editor.operator_count = count(params);
        }
        Some(name) => {
            let count = count(params);
            if let Some(span) = motion_span(editor, name, count) {
                editor.change_case(span.start, span.end, change);
            }
        }
    }
}

/// The shape the doubled case keys have: whole lines, as many as the count says.
///
/// The lines' terminators are inside the range a linewise operator takes, and
/// they are not letters, so a case change over them cannot touch them: the range
/// is cut back to the last line's text.
fn case_lines(editor: &mut Editor, params: &[Param], change: CaseChange) {
    let (start, end) = line_span(editor, editor.cursor.row, count(params));
    let last = editor.buffer.char_to_line_col(end.saturating_sub(1)).0;
    let text_end = editor.buffer.line_col_to_char(last, 0) + editor.buffer.line_len(last);
    editor.change_case(start, text_end.max(start), change);
}

/// `>` — one level further in, over the lines a motion names.
fn indent_motion(editor: &mut Editor, params: &[Param]) -> Result<()> {
    shift_motion(editor, params, Operator::Indent)
}

/// `<` — one level back out.
fn outdent_motion(editor: &mut Editor, params: &[Param]) -> Result<()> {
    shift_motion(editor, params, Operator::Outdent)
}

fn shift_motion(editor: &mut Editor, params: &[Param], operator: Operator) -> Result<()> {
    match motion_param(params) {
        None => {
            editor.operator = Some(operator);
            editor.operator_count = count(params);
        }
        Some(name) => {
            let count = count(params);
            if let Some(span) = motion_span(editor, name, count) {
                let from = editor.buffer.char_to_line_col(span.start).0;
                let to = editor.buffer.char_to_line_col(span.end.saturating_sub(1)).0;
                editor.shift_indent(from, to, operator == Operator::Indent);
            }
        }
    }
    Ok(())
}

/// `>>` — one level further in, whole lines.
fn indent_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let (start, end) = line_span(editor, editor.cursor.row, count(params));
    editor.shift_indent(
        editor.buffer.char_to_line_col(start).0,
        editor.buffer.char_to_line_col(end.saturating_sub(1)).0,
        true,
    );
    Ok(())
}

/// `<<` — one level back out, whole lines.
fn outdent_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let (start, end) = line_span(editor, editor.cursor.row, count(params));
    editor.shift_indent(
        editor.buffer.char_to_line_col(start).0,
        editor.buffer.char_to_line_col(end.saturating_sub(1)).0,
        false,
    );
    Ok(())
}

/// `=` — reindent the lines the motion names.
fn reindent_motion(editor: &mut Editor, params: &[Param]) -> Result<()> {
    match motion_param(params) {
        None => {
            editor.operator = Some(Operator::Reindent);
            editor.operator_count = count(params);
        }
        Some(name) => {
            let count = count(params);
            if let Some(span) = motion_span(editor, name, count) {
                let from = editor.buffer.char_to_line_col(span.start).0;
                let to = editor.buffer.char_to_line_col(span.end.saturating_sub(1)).0;
                let lines = editor.reindent_lines(from, to);
                editor.message = format!("{lines} lines reindented");
            }
        }
    }
    Ok(())
}

/// `==` — reindent whole lines.
fn reindent_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let (start, end) = line_span(editor, editor.cursor.row, count(params));
    let lines = editor.reindent_lines(
        editor.buffer.char_to_line_col(start).0,
        editor.buffer.char_to_line_col(end.saturating_sub(1)).0,
    );
    editor.message = format!("{lines} lines reindented");
    Ok(())
}

/// `gq` — rewrap the lines the motion names.
fn format_motion(editor: &mut Editor, params: &[Param]) -> Result<()> {
    match motion_param(params) {
        None => {
            editor.operator = Some(Operator::Format);
            editor.operator_count = count(params);
        }
        Some(name) => {
            let count = count(params);
            if let Some(span) = motion_span(editor, name, count) {
                let lines = editor.format_lines(
                    editor.buffer.char_to_line_col(span.start).0,
                    editor.buffer.char_to_line_col(span.end.saturating_sub(1)).0,
                );
                editor.message = format!("{lines} lines rewrapped");
            }
        }
    }
    Ok(())
}

/// `gqq` — rewrap whole lines.
fn format_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let (start, end) = line_span(editor, editor.cursor.row, count(params));
    let lines = editor.format_lines(
        editor.buffer.char_to_line_col(start).0,
        editor.buffer.char_to_line_col(end.saturating_sub(1)).0,
    );
    editor.message = format!("{lines} lines rewrapped");
    Ok(())
}

/// `g~` — swap the case of the range the motion names.
fn toggle_case_motion(editor: &mut Editor, params: &[Param]) -> Result<()> {
    case_motion(editor, params, CaseChange::Toggle, Operator::ToggleCase);
    Ok(())
}

/// `gu` — lower case the range.
fn lower_motion(editor: &mut Editor, params: &[Param]) -> Result<()> {
    case_motion(editor, params, CaseChange::Lower, Operator::Lower);
    Ok(())
}

/// `gU` — upper case the range.
fn upper_motion(editor: &mut Editor, params: &[Param]) -> Result<()> {
    case_motion(editor, params, CaseChange::Upper, Operator::Upper);
    Ok(())
}

/// `g~~` — swap the case of whole lines.
fn toggle_case_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    case_lines(editor, params, CaseChange::Toggle);
    Ok(())
}

/// `guu` — lower case whole lines.
fn lower_case_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    case_lines(editor, params, CaseChange::Lower);
    Ok(())
}

/// `gUU` — upper case whole lines.
fn upper_case_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    case_lines(editor, params, CaseChange::Upper);
    Ok(())
}

/// `c` — take a range out and insert where it was, or start waiting for its
/// motion.
fn change_motion(editor: &mut Editor, params: &[Param]) -> Result<()> {
    match motion_param(params) {
        None => {
            editor.operator = Some(Operator::Change);
            editor.operator_count = count(params);
        }
        Some(name) => {
            let count = count(params);
            let Some(mut span) = motion_span(editor, name, count) else {
                return Ok(());
            };
            if name == "move_word_forward" {
                // `cw` stops at the end of the word under the cursor rather
                // than at the start of the next one, which is `ce` in disguise.
                let word_end = word_run_end(editor, span.start);
                let moved = span.end > span.start;
                span.end = word_end.max(if moved { span.start + 1 } else { span.start });
            }
            let kind = span_kind(&span);
            let end = change_end(editor, &span);
            // Insert mode first: the cut clamps the cursor for normal mode, and
            // what is typed next belongs where the range started.
            editor.mode = Mode::Insert;
            editor.cut(span.start, end, kind, Some(span.start));
            editor.cursor.set_char_idx(&editor.buffer, span.start);
        }
    }
    Ok(())
}

fn span_kind(span: &Span) -> Kind {
    if span.linewise {
        Kind::Linewise
    } else {
        Kind::Charwise
    }
}

/// `<C-c>` — put the selection, or the register, on the system clipboard.
///
/// The kernel has no clipboard; this hands the text to the daemon, which hands
/// it to a frontend that does. Vim spells this `"+y`, and a terminal spells it
/// Ctrl+Shift+C, but there is no key a window and a terminal both give you.
fn copy(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.request_copy();
    Ok(())
}

/// `p` — put the register after the cursor, or below the line.
fn put_after(editor: &mut Editor, params: &[Param]) -> Result<()> {
    put(editor, params, true)
}

/// `P` — put the register before the cursor, or above the line.
fn put_before(editor: &mut Editor, params: &[Param]) -> Result<()> {
    put(editor, params, false)
}

/// Put what the register holds, `count` times.
///
/// Where it goes depends on what it is, which is why the register remembers:
/// whole lines go above or below the cursor's line, characters go into the line
/// at the cursor. Either way it is one [`Editor::replace_range`] per repeat, so
/// one `u` takes the whole paste back.
fn put(editor: &mut Editor, params: &[Param], after: bool) -> Result<()> {
    let register = editor.chosen_register();
    if register.is_empty() {
        // Nothing has been taken yet. Vim beeps and changes nothing.
        return Ok(());
    }
    let text = register.text().to_string();
    let linewise = register.is_linewise();

    // A rectangle goes back in columns: one row of the register per row of the
    // buffer, starting at the cursor's column — after it for `p`, at it for `P`.
    // A row that is too short is *padded* with spaces, which is vim's rule and
    // the only place in this editor that invents text: a column is a place, and
    // a place past the end of a line is a space.
    if register.is_blockwise() {
        let rows: Vec<String> = register.rows().iter().map(|row| row.to_string()).collect();
        // A count repeats the rectangle *sideways*, which is what vim does with
        // one: the columns go in, then the same columns again after them.
        let width = rows
            .iter()
            .map(|row| row.chars().count())
            .max()
            .unwrap_or(0);
        let first_col = editor.cursor.col + usize::from(after);
        let first_row = editor.cursor.row;
        let last_row = editor.buffer.line_count().saturating_sub(1);
        let mut caret = None;
        for repeat in 0..count(params) {
            let start_col = first_col + repeat * width;
            for (offset, row_text) in rows.iter().enumerate() {
                let row = first_row + offset;
                if row > last_row {
                    break;
                }
                let len = editor.buffer.line_len(row);
                let base = editor.buffer.line_col_to_char(row, 0);
                let at = base + start_col.min(len);
                // The padding is what the line is missing, and nothing when it is
                // long enough already.
                let pad = " ".repeat(start_col.saturating_sub(len));
                let insert = format!("{pad}{row_text}");
                let end = at + insert.chars().count();
                editor.replace_range(at, at, &insert, Some(end.saturating_sub(1)));
                if caret.is_none() {
                    caret = Some(end.saturating_sub(1));
                }
            }
        }
        // Vim leaves the cursor on the last character of the *first* row it put.
        if let Some(caret) = caret {
            editor.cursor.set_char_idx(&editor.buffer, caret);
        }
        return Ok(());
    }

    for _ in 0..count(params) {
        if linewise {
            let row = editor.cursor.row;
            let at = if after {
                // The start of the line below: the paste goes between them.
                editor
                    .buffer
                    .line_col_to_char(row, editor.buffer.line_len(row))
                    + editor.buffer.line_terminator_len(row)
            } else {
                editor.buffer.line_col_to_char(row, 0)
            };
            editor.replace_range(at, at, &text, Some(at));
        } else {
            let idx = editor.cursor.char_idx(&editor.buffer);
            let at = if after {
                // After the character under the cursor, but never past the end
                // of the line: `p` appends to the line it is on.
                let line_end = editor
                    .buffer
                    .line_col_to_char(editor.cursor.row, editor.buffer.line_len(editor.cursor.row));
                editor.buffer.next_grapheme_boundary(idx).min(line_end)
            } else {
                idx
            };
            let end = at + text.chars().count();
            // Vim leaves the cursor on the last character it put.
            editor.replace_range(at, at, &text, Some(end.saturating_sub(1)));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Visual mode
// ---------------------------------------------------------------------------

/// `v` — start a characterwise selection, change what one covers, or leave.
fn enter_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    visual(editor, VisualKind::Charwise);
    Ok(())
}

/// `V` — the same, for whole lines.
fn enter_visual_line(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    visual(editor, VisualKind::Linewise);
    Ok(())
}

/// What `v` and `V` mean once one of them has been pressed.
///
/// The same kind again leaves visual mode, the other kind changes what the
/// selection covers: vim's toggle, and the reason both keys are bound in both
/// modes.
fn visual(editor: &mut Editor, kind: VisualKind) {
    match editor.selection {
        Some(selection) if selection.kind == kind => {
            editor.selection = None;
            editor.mode = Mode::Normal;
        }
        Some(selection) => editor.selection = Some(Selection { kind, ..selection }),
        None => {
            editor.selection = Some(Selection {
                anchor: editor.cursor,
                kind,
                to_end: false,
            });
            editor.mode = Mode::Visual;
        }
    }
}

/// `Esc` in visual mode: drop the selection and go back to normal.
fn escape_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    // The selection is remembered by `clear_pending`, which is why the mode is
    // changed after it rather than before — and why the selection is let go here,
    // since `clear_pending` leaves a visual selection alone while the mode is
    // still visual.
    editor.clear_pending();
    editor.selection = None;
    editor.mode = Mode::Normal;
    Ok(())
}

/// `o` — put the cursor on the other end of the selection.
fn swap_ends(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if let Some(selection) = editor.selection {
        editor.selection = Some(Selection {
            anchor: editor.cursor,
            ..selection
        });
        editor.cursor = selection.anchor;
        editor.after_edit();
    }
    Ok(())
}

/// What an operator does with a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionAction {
    /// Take it into the register.
    Delete,
    /// Copy it into the register.
    Yank,
    /// Take it into the register and start typing where it was.
    Change,
}

/// `$` over a block — the right edge becomes the end of each line.
///
/// A rectangle has a right *column*; this makes the block ragged, which is what
/// `<C-v>$d` means: "from here to the end of every line", not "this many columns".
/// On any other kind of selection `$` is the line-end motion it always was.
fn block_to_end(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let Some(selection) = editor.selection else {
        return move_line_end(editor, &[]);
    };
    if selection.kind != VisualKind::Blockwise {
        return move_line_end(editor, &[]);
    }
    editor.selection = Some(Selection {
        to_end: true,
        ..selection
    });
    editor.cursor.move_line_end(&editor.buffer);
    editor.after_edit();
    Ok(())
}

/// `q:` — what has been typed on the `:` line, in a window of its own.
fn command_history(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.open_history_window(false);
    Ok(())
}

/// `q/` — the same for what has been searched for.
fn search_history(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.open_history_window(true);
    Ok(())
}

/// `J` — join the cursor's line with the next one, `count` lines in all.
///
/// A count of one is still two lines: vim's rule, and the reason `J` on its own
/// joins the line with the one below it.
fn join(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    let count = count(params).max(2);
    let joined = editor.join_lines(row, row + count - 1);
    if joined == 0 {
        editor.message = "E85: There is no line to join".to_string();
    }
    Ok(())
}

/// `gJ` — the same, joining without inserting or removing any spaces.
fn join_tight(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    let count = count(params).max(2);
    let joined = editor.join_lines_tight(row, row + count - 1);
    if joined == 0 {
        editor.message = "E85: There is no line to join".to_string();
    }
    Ok(())
}

/// `gJ` over a selection — every line of it.
fn join_tight_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let Some((start, end)) = editor.visual_span() else {
        editor.clear_pending();
        return Ok(());
    };
    let first = editor.buffer.char_to_line_col(start).0;
    let last = editor.buffer.char_to_line_col(end.saturating_sub(1)).0;
    editor.clear_pending();
    editor.selection = None;
    editor.mode = Mode::Normal;
    editor.join_lines_tight(first, last);
    Ok(())
}

/// `J` over a selection — every line of it, which is what the whole-lines note in
/// vim's table means.
fn join_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let Some((start, end)) = editor.visual_span() else {
        editor.clear_pending();
        return Ok(());
    };
    let first = editor.buffer.char_to_line_col(start).0;
    let last = editor.buffer.char_to_line_col(end.saturating_sub(1)).0;
    editor.clear_pending();
    editor.selection = None;
    editor.mode = Mode::Normal;
    editor.join_lines(first, last);
    Ok(())
}

/// `zf{motion}` — fold the lines the motion covers.
///
/// An operator like `d` and `y`: without a motion it waits for one, and with one it
/// folds the range — the whole lines the motion touches, since a fold is a range of
/// lines and not of characters.
fn fold_motion(editor: &mut Editor, params: &[Param]) -> Result<()> {
    match motion_param(params) {
        None => {
            editor.operator = Some(Operator::Fold);
            editor.operator_count = count(params);
        }
        Some(name) => {
            let count = count(params);
            if let Some(span) = motion_span(editor, name, count) {
                let first = editor.buffer.char_to_line_col(span.start).0;
                let last = editor.buffer.char_to_line_col(span.end.saturating_sub(1)).0;
                editor.fold_lines(first, last);
            }
        }
    }
    Ok(())
}

/// `zff` — fold the cursor's line, and the `count - 1` below it.
fn fold_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    editor.fold_lines(row, row + count(params).saturating_sub(1));
    Ok(())
}

/// `zo` — open the fold the cursor is in.
fn fold_open(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.set_fold_closed(false);
    Ok(())
}

/// `zc` — close it.
fn fold_close(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.set_fold_closed(true);
    Ok(())
}

/// `zR` — open every fold.
fn fold_open_all(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.set_all_folds_closed(false);
    Ok(())
}

/// `zM` — close every fold.
fn fold_close_all(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.set_all_folds_closed(true);
    Ok(())
}

/// `:mkview` — keep this window's folds and position for later.
fn make_view(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    match editor.make_view() {
        Ok(()) => Ok(()),
        Err(error) => {
            editor.message = format!("E474: {error}");
            Err(CommandError::Failed(error.to_string()))
        }
    }
}

/// `:loadview` — read them back.
fn load_view(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    match editor.load_view() {
        Ok(()) => Ok(()),
        Err(error) => {
            editor.message = format!("E474: {error}");
            Err(CommandError::Failed(error.to_string()))
        }
    }
}

/// `zm` — show one level of folds fewer.
fn fold_level_less(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.step_fold_level(false);
    Ok(())
}

/// `zr` — one level more.
fn fold_level_more(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.step_fold_level(true);
    Ok(())
}

/// `zx` — work the folds out again, which is what `'foldmethod'` needs when the
/// text has changed shape under it.
fn fold_update(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if !editor.foldmethod_is_indent() {
        editor.message = "E490: No fold found".to_string();
        return Ok(());
    }
    let count = editor.compute_indent_folds();
    editor.message = format!("{count} fold{}", if count == 1 { "" } else { "s" });
    Ok(())
}

/// `zj` — to the start of the next fold.
fn fold_next(editor: &mut Editor, params: &[Param]) -> Result<()> {
    editor.goto_next_fold(count(params));
    Ok(())
}

/// `zk` — to the start of the one before it.
fn fold_previous(editor: &mut Editor, params: &[Param]) -> Result<()> {
    editor.goto_previous_fold(count(params));
    Ok(())
}

/// `zd` — take the fold the cursor is in away.
fn fold_delete(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.delete_fold_at_cursor();
    Ok(())
}

/// `gf` — open the file whose name is under the cursor.
///
/// The name is read the way vim reads it, by what a file name is *made of* rather
/// than by what a word is: `src/main.rs` is one file name and three words, and a
/// `gf` that took the word would open `main`.
fn goto_file(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let Some(name) = file_name_under_cursor(editor) else {
        editor.message = "E484: No file name under the cursor".to_string();
        return Ok(());
    };
    // Relative to the file that is open first, which is what a name in a file
    // means: a name beside it. Then `'path'`, in the order it is written — which is
    // what the setting is for, and how a name in an `import` line finds its way to
    // a file in another directory. A buffer with no file has no directory, so the
    // name stands on its own before the search.
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    match editor.buffer.path().and_then(Path::parent) {
        Some(dir) => candidates.push(dir.join(&name)),
        None => candidates.push(std::path::PathBuf::from(&name)),
    }
    for dir in editor.options.borrow().path.0.iter() {
        candidates.push(dir.join(&name));
    }
    let Some(path) = candidates.into_iter().find(|path| path.is_file()) else {
        editor.message = format!("E484: Can't open file {name}");
        return Ok(());
    };
    // A name that is not there is refused rather than opened: `:e` makes a new
    // file out of a name a person typed, and `gf` is a jump — vim's rule, and the
    // reason `gf` on a name that has been deleted says so.
    if !path.is_file() {
        editor.message = format!("E484: Can't open file {name}");
        return Ok(());
    }
    match editor.open_file(&path) {
        Ok(()) => editor.message = format!("\"{}\"", path.display()),
        Err(_) => editor.message = format!("E484: Can't open file {name}"),
    }
    Ok(())
}

/// The run of file-name characters the cursor is on, if it is on one.
///
/// This is vim's `isfname` in spirit: letters, digits, and the punctuation a path
/// is made of — `_`, `-`, `.`, `/`, `\`, `~`, `:`. Anything else ends the name, so
/// `see src/main.rs.` gives `src/main.rs` rather than the full stop after it.
fn file_name_under_cursor(editor: &Editor) -> Option<String> {
    let buffer = &editor.buffer;
    let at = editor.cursor.char_idx(buffer);
    let is_name_char =
        |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\' | '~' | ':');
    if !buffer.char_at(at).is_some_and(is_name_char) {
        return None;
    }
    let mut start = at;
    while start > 0 && buffer.char_at(start - 1).is_some_and(is_name_char) {
        start -= 1;
    }
    let mut end = at + 1;
    while buffer.char_at(end).is_some_and(is_name_char) {
        end += 1;
    }
    let name = buffer.slice_text(start, end);
    // A name has to have something in it that is not punctuation: `...` is not a
    // file, and neither is a lone `/`.
    let has_content = name.chars().any(|c| c.is_alphanumeric());
    (has_content && !name.contains('\n')).then_some(name)
}

/// `I` over a block — the same typing at the left edge of every row.
fn insert_block_left(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    block_insert_at_edge(editor, 0);
    Ok(())
}

/// `A` over a block — the same typing just past the right edge of every row.
fn insert_block_right(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    block_insert_at_edge(editor, 1);
    Ok(())
}

/// Start typing at one of a block's edges, on every row of it.
///
/// `past` is how far past the block's left column the typing starts: `I` types
/// *at* it and `A` one past the right one, which is what makes `A` append.
fn block_insert_at_edge(editor: &mut Editor, past: usize) {
    let Some((first, last, left, right)) = editor.visual_block() else {
        editor.clear_pending();
        return;
    };
    // The rows the typing goes on are decided by the block's *shape*, before
    // anything is deleted: a line that does not reach the block is not part of it
    // (`v_b_I`), and `A` — which appends — takes every row and pads the short ones.
    let rows: Vec<usize> = (first..=last)
        .filter(|row| past != 0 || editor.buffer.line_len(*row) > left)
        .collect();
    // `A` on a block that reaches the end of each line types at the end of each
    // line, which is what the ragged shape means. `I` types at the start of the
    // block, and a line that does not reach it stays as it is (`v_b_I`); `A`
    // appends, and pads a short line out to the block's edge (`v_b_A`).
    let col = if past == 0 {
        Some(left)
    } else {
        right.map(|right| right + 1)
    };
    // `clear_pending` remembers the selection for `gv`, so it runs while the
    // mode is still visual — the order every other visual command uses.
    editor.clear_pending();
    editor.selection = None;
    editor.start_block_insert(rows, col, past != 0);
}

/// `<C-v>` — a rectangle: the rows between the two ends, and the columns
/// between them.
fn visual_block(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    // (With an operator pending, `<C-v>` never gets here: the operator's own
    // handler answers it, since it is not a motion — see `o_CTRL-V`.)
    visual(editor, VisualKind::Blockwise);
    Ok(())
}

/// `d` over a selection.
fn delete_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    apply_to_selection(editor, SelectionAction::Delete);
    Ok(())
}

/// `y` over a selection.
fn yank_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    apply_to_selection(editor, SelectionAction::Yank);
    Ok(())
}

/// `c` over a selection.
fn change_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    apply_to_selection(editor, SelectionAction::Change);
    Ok(())
}

/// `>` over a selection: one level further in, and back to normal mode.
fn indent_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    shift_visual(editor, true);
    Ok(())
}

/// `<` over a selection: one level back out.
fn outdent_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    shift_visual(editor, false);
    Ok(())
}

/// `=` over a selection: reindent by what the lines open and close.
fn reindent_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let Some((start, end)) = editor.visual_span() else {
        editor.clear_pending();
        return Ok(());
    };
    let from = editor.buffer.char_to_line_col(start).0;
    let to = editor.buffer.char_to_line_col(end.saturating_sub(1)).0;
    // The selection is remembered for `gv` by `clear_pending`, which is why the
    // mode changes after it.
    editor.clear_pending();
    editor.selection = None;
    editor.mode = Mode::Normal;
    editor.reindent_lines(from, to);
    Ok(())
}

/// Move the lines a selection covers one level in or out, and leave visual mode.
fn shift_visual(editor: &mut Editor, forward: bool) {
    let Some((start, end)) = editor.visual_span() else {
        editor.clear_pending();
        return;
    };
    let from = editor.buffer.char_to_line_col(start).0;
    let to = editor.buffer.char_to_line_col(end.saturating_sub(1)).0;
    editor.clear_pending();
    editor.selection = None;
    editor.mode = Mode::Normal;
    editor.shift_indent(from, to, forward);
}

/// `U` over a selection: upper case, and back to normal mode.
fn upper_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    case_visual(editor, CaseChange::Upper);
    Ok(())
}

/// `u` over a selection: lower case. Vim's visual `u` is not undo, which is what
/// the normal-mode `u` is for.
fn lower_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    case_visual(editor, CaseChange::Lower);
    Ok(())
}

/// `~` over a selection: the case of every letter, swapped.
fn toggle_case_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    case_visual(editor, CaseChange::Toggle);
    Ok(())
}

/// Change the case of the selection, and leave visual mode.
fn case_visual(editor: &mut Editor, change: CaseChange) {
    if let Some((start, end)) = editor.visual_span() {
        editor.change_case(start, end, change);
    }
    editor.clear_pending();
    editor.mode = Mode::Normal;
    editor.selection = None;
}

/// Act on the selection, and leave visual mode.
fn apply_to_selection(editor: &mut Editor, action: SelectionAction) {
    // A rectangle is not one range: it is one range per row, and what a person
    // means by `d` on it is all of them. The kernel owns that, since the
    // register is its business — and the whole thing is one command, which is
    // one step in the history.
    if editor
        .selection
        .is_some_and(|selection| selection.kind == VisualKind::Blockwise)
    {
        let rectangle = editor.visual_block();
        // Which rows a change touches is a question about the block's *shape*,
        // asked before anything is deleted: a line that does not reach the block
        // stays as it is (`v_b_I`), and asking after the deletion would say every
        // line had shrunk out of it.
        let typed_on: Vec<usize> = match rectangle {
            Some((first, last, _, _)) => {
                let spans = editor.block_spans();
                (first..=last)
                    .filter(|row| {
                        spans
                            .get(row - first)
                            .is_some_and(|(start, end)| start < end)
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        // `c` types where the block was, and the typing belongs to the insert
        // session. The session's group opens *after* this command returns — when
        // the mode change is noticed — so the deletion would land in a group of
        // its own and come apart from the typing under `u`. Opening a group here
        // nests the session's inside it, which is what keeps the two together.
        if action == SelectionAction::Change {
            editor.mode = Mode::Insert;
            editor.history.begin_group();
        }
        let (row, col) = match action {
            SelectionAction::Yank => editor.copy_block(),
            SelectionAction::Delete | SelectionAction::Change => editor.cut_block(),
        };
        editor.cursor.row = row;
        editor.cursor.col = col;
        // `clear_pending` remembers the selection for `gv`, so it runs while the
        // mode is still visual — the same order the charwise path uses.
        editor.clear_pending();
        editor.selection = None;
        editor.mode = Mode::Normal;
        editor.after_edit();
        // `c` on a block types on every row of it, in the place the block was.
        if action == SelectionAction::Change {
            if let Some((_, _, left, _)) = rectangle {
                editor.start_block_insert(typed_on, Some(left), false);
            }
        }
        return;
    }
    let Some((start, end)) = editor.visual_span() else {
        editor.clear_pending();
        return;
    };
    let linewise = editor
        .selection
        .is_some_and(|selection| selection.kind == VisualKind::Linewise);
    let kind = if linewise {
        Kind::Linewise
    } else {
        Kind::Charwise
    };
    let span = Span {
        start,
        end,
        linewise,
    };
    match action {
        SelectionAction::Delete => editor.cut(start, end, kind, Some(start)),
        SelectionAction::Yank => {
            editor.copy(start, end, kind);
            // Vim leaves the cursor where the yanked text started.
            editor.cursor.set_char_idx(&editor.buffer, start);
            editor.after_edit();
        }
        SelectionAction::Change => {
            let end = change_end(editor, &span);
            editor.cut(start, end, kind, Some(start));
        }
    }
    // `clear_pending` is what remembers the selection for `gv`, so it runs while
    // the mode is still visual — and the mode changes after it, not before.
    if action == SelectionAction::Change {
        editor.clear_pending();
        editor.selection = None;
        editor.mode = Mode::Insert;
    } else {
        editor.clear_pending();
        editor.selection = None;
        editor.mode = Mode::Normal;
    }
}

/// `p` over a selection: put the register where the selection is.
///
/// The replaced text is *not* what goes into the register — a departure from
/// vim, where `p` over a selection swaps the two, which surprises everybody the
/// first time. The register is left exactly as it was.
fn put_visual(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let Some((start, end)) = editor.visual_span() else {
        editor.clear_pending();
        return Ok(());
    };
    let text = editor.register.text().to_string();
    editor.selection = None;
    editor.mode = Mode::Normal;
    editor.clear_pending();
    if text.is_empty() {
        editor.replace_range(start, end, "", Some(start));
        return Ok(());
    }
    let after = start + text.chars().count() - 1;
    editor.replace_range(start, end, &text, Some(after));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_present() {
        let registry = CommandRegistry::with_builtins();
        for name in [
            "delete_line",
            "save_quit",
            "edit_file_force",
            "page_down",
            "move_word_backward",
        ] {
            assert!(registry.contains(name), "{name} is missing");
        }
        assert!(!registry.contains("nope"));
        // Every default binding must resolve to a real command.
        let keymap = crate::keymap::Keymap::with_defaults();
        for (_, _, commands) in keymap.bindings() {
            for command in commands {
                assert!(registry.contains(&command), "unbound command: {command}");
            }
        }
    }

    #[test]
    fn unknown_commands_are_reported() {
        let registry = CommandRegistry::with_builtins();
        let mut editor = Editor::for_tests();
        let err = registry.run(&mut editor, "nope", &[]).unwrap_err();
        assert_eq!(err, CommandError::Unknown("nope".into()));
        assert_eq!(err.to_string(), "E492: Not a command: nope");
    }

    #[test]
    fn count_defaults_to_one() {
        assert_eq!(count(&[]), 1);
        assert_eq!(count(&[Param::Int(3)]), 3);
        assert_eq!(count(&[Param::Int(0)]), 1);
        assert_eq!(count(&[Param::Int(-2)]), 1);
        assert_eq!(count(&[Param::Str("x".into())]), 1);
    }
}
