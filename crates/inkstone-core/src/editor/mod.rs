//! The editor: state, the mode state machine, and the command dispatch loop.
//!
//! `Editor` is the *only* thing the daemon owns. It has no I/O, no threads and
//! no terminal: it consumes key presses and produces a [`Composed`] screen, so
//! the whole editor is exercisable from a unit test.
//!
//! Dispatch order for a named command:
//!
//! 1. the built-in [`CommandRegistry`],
//! 2. a command registered from Lua,
//! 3. otherwise an `E492` message.
//!
//! Lua callbacks cannot borrow the editor, so `ed.run(...)` inside a script
//! *queues* the call; [`Editor::run_command`] drains that queue after every
//! command, which is what makes `ed.map("n", "x", function() ed.run("move_right") end)`
//! work without re-entrancy.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use inkstone_rpc::{KeyCode, KeyMsg, Param};

use crate::buffer::{Buffer, BufferError};
use crate::changes::Changes;
use crate::command::{CommandError, CommandRegistry};
use crate::completion::{self, Item as CompletionItem};
use crate::cursor::Cursor;
use crate::diagnostic::{order as diagnostic_order, Diagnostic};
use crate::highlight::{Language, LineStates};
use crate::history::History;
use crate::jumps::Jumps;
use crate::keymap::Keymap;
use crate::lua::LuaHost;
use crate::marks::{MarkJump, Marks};
use crate::mode::{Mode, VisualKind};
use crate::register::{Prefix, Register, Registers};
use crate::screen::Viewport;
use crate::search::{Direction, Search};
use crate::settings::Options;
use crate::substitute::Substitute;
use crate::CoreError;

// The editor is one type in several files: every concern gets its own `impl`
// block, which is how a 6,000-line impl stays readable — the type is deep, the
// file is not the module.
mod confirm;
mod documents;
mod edits;
mod ex;
mod folds;
mod insert;
mod jobs;
mod keys;
mod lists;
mod lsp;
mod options;
mod pointer;
mod prompt;
mod view;
mod visual;
mod windows;

/// Guard against a Lua command calling itself forever.
pub(crate) const MAX_COMMAND_DEPTH: usize = 100;

/// How many lines one notch of the wheel scrolls the view.
pub(crate) const MOUSE_SCROLL_LINES: i64 = 3;

/// How many digits a count may have. Long enough for any count a person types,
/// short enough that a leaned-on key cannot overflow one.
pub(crate) const MAX_COUNT_DIGITS: usize = 9;

/// What the line at the bottom of the screen is asking for.
///
/// One mode for all three, because a `:` command and a `/` search are the same
/// thing to type into: a line with a cursor, a backspace and an escape. Only the
/// first character differs, and what running it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prompt {
    /// `:` — a command.
    Command,
    /// `/` — a search, forwards.
    SearchForward,
    /// `?` — a search, backwards.
    SearchBackward,
}

impl Prompt {
    /// The character the prompt is spelled with.
    pub fn spelling(self) -> char {
        match self {
            Prompt::Command => ':',
            Prompt::SearchForward => '/',
            Prompt::SearchBackward => '?',
        }
    }

    /// Which way a search entered at this prompt runs.
    pub(crate) fn direction(self) -> Option<Direction> {
        match self {
            Prompt::Command => None,
            Prompt::SearchForward => Some(Direction::Forward),
            Prompt::SearchBackward => Some(Direction::Backward),
        }
    }
}

/// Which value a `:set` writes, which is the manual's table:
///
/// ```text
///       :set option=value       both      both
///  :setlocal option=value        -        set
/// :setglobal option=value       set        -
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// `:set` — the value in force here, and the one every other file gets.
    Both,
    /// `:setlocal` — only the file that is open.
    Local,
    /// `:setglobal` — only the value outside any file.
    Global,
}

/// A search being typed, shown as it goes — `'incsearch'`.
///
/// The pattern is run for real as it is typed, so the highlight on screen is
/// the one being typed and not the last one's; what was in force before is kept
/// beside it, because a search that is cancelled has to put it back.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IncSearch {
    /// Where the cursor was when the search started: where a cancelled one
    /// leaves it, and where each keystroke searches from again.
    start: Cursor,
    /// The search that was in force, to put back if this one is cancelled.
    previous: Option<(String, Direction)>,
    /// Whether a match is on screen right now — the cursor goes to it, and the
    /// command line stops holding the cursor while it is.
    showing: bool,
}

/// A visual selection: `v` or `V` was pressed, and the cursor is one end of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Selection {
    /// Where the selection started.
    pub(crate) anchor: Cursor,
    /// Whether it covers characters or whole lines.
    pub kind: VisualKind,
    /// A blockwise selection whose right edge is the end of each line rather than
    /// a column: `<C-v>$` makes a *ragged* block, which is what `d` on it means —
    /// "from here to the end of every line", not "this many columns".
    pub(crate) to_end: bool,
}

/// The operator a `d`, `y` or `c` started, waiting for the motion it works on.
///
/// What a macro key is waiting for: `q` for a register to record into (or to stop
/// recording), and `@` for one to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MacroAsk {
    /// `q` — the next key names the register to record into, or ends the recording.
    Record,
    /// `@` — the next key names the register to play.
    Play,
}

/// How an operator's motion was forced to behave, by `v`, `V` or `<C-v>` typed
/// after the operator — vim's `o_v`, `o_V` and `o_CTRL-V`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForcedShape {
    /// `v`: characters, whatever the motion was. A linewise motion becomes
    /// *exclusive*; a characterwise one has its inclusive/exclusive the other way
    /// round — which is what makes `dvj` and `dv$` do what they say.
    Charwise,
    /// `V`: whole lines, whatever the motion was.
    Linewise,
    /// `<C-v>`: a block, whose corners are the cursor before and after the motion.
    Blockwise,
}

/// The three differ only in what they do with the range once it is resolved, so
/// they are one enum rather than three pieces of pending state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    /// Take the range out, and into the register.
    Delete,
    /// Copy the range into the register, leaving the text alone.
    Yank,
    /// Take the range out, and start inserting where it was.
    Change,
    /// Rewrap the range's lines to `textwidth`.
    Format,
    /// Reindent the range's lines by what they open and close.
    Reindent,
    /// One level further in.
    Indent,
    /// One level back out.
    Outdent,
    /// Toggle the case of the range, leaving it in place.
    ToggleCase,
    /// Make the range lower case.
    Lower,
    /// Make the range upper case.
    Upper,
    /// Fold the range's lines into one closed fold.
    Fold,
}

impl Operator {
    /// The command that starts this operator, and applies it to a motion.
    /// The command that does this operator to the *visual selection*.
    ///
    /// The two differ only in where the range comes from, so an operator forced
    /// to be blockwise (`d<C-V>`) makes a selection out of the rectangle and runs
    /// this — the same code every other block operator goes through.
    pub(crate) fn visual_command(self) -> &'static str {
        match self {
            Operator::Delete => "delete_visual",
            Operator::Yank => "yank_visual",
            Operator::Change => "change_visual",
            _ => self.command(),
        }
    }

    /// The command that does this operator to the range a *motion* makes.
    pub fn command(self) -> &'static str {
        match self {
            Operator::Delete => "delete_motion",
            Operator::Yank => "yank_motion",
            Operator::Change => "change_motion",
            Operator::Format => "format_motion",
            Operator::Reindent => "reindent_motion",
            Operator::Indent => "indent_motion",
            Operator::Outdent => "outdent_motion",
            Operator::ToggleCase => "toggle_case_motion",
            Operator::Lower => "lower_motion",
            Operator::Upper => "upper_motion",
            Operator::Fold => "fold_motion",
        }
    }

    /// The key that doubles to mean whole lines: `d`, `y`, `c`, and for the
    /// case operators the key they end on, since vim spells those `gUU`, `guu`
    /// and `g~~`.
    pub fn key(self) -> &'static str {
        match self {
            Operator::Delete => "d",
            Operator::Yank => "y",
            Operator::Change => "c",
            Operator::Format => "q",
            Operator::Reindent => "=",
            // The canonical spelling, which is what a key press encodes to:
            // `encode_key` writes a literal angle bracket as `<gt>`, so that is
            // what the doubled key has to be compared against.
            Operator::Indent => "<gt>",
            Operator::Outdent => "<lt>",
            Operator::ToggleCase => "~",
            Operator::Lower => "u",
            Operator::Upper => "U",
            // A fold has no doubled key: `zff` is the line form, and there is
            // nothing else it could be spelled as.
            Operator::Fold => "zf",
        }
    }

    /// The command for the doubled key: `dd`, `yy`, `cc`.
    pub(crate) fn line_command(self) -> &'static str {
        match self {
            Operator::Delete => "delete_line",
            Operator::Yank => "yank_line",
            Operator::Change => "change_line",
            Operator::Format => "format_line",
            Operator::Reindent => "reindent_line",
            Operator::Indent => "indent_line",
            Operator::Outdent => "outdent_line",
            Operator::ToggleCase => "toggle_case_line",
            Operator::Lower => "lower_case_line",
            Operator::Upper => "upper_case_line",
            Operator::Fold => "fold_line",
        }
    }
}

/// The editor: the document, the cursor, the mode and everything the commands
/// operate on.
///
/// It does no I/O and owns no terminal, so a test can drive it key by key and
/// inspect the screen it composes.
/// One open file, and everything that belongs to *it* rather than to the
/// editor: where the cursor is, what can be undone, which search is up, what has
/// been coloured, and where the window is looking.
///
/// While a document is on screen its state lives in the editor's own fields and
/// this struct holds only a placeholder; the two are swapped when another
/// document takes its place. That swap is the whole of switching buffers, which
/// is why nothing else in this file had to change to have more than one.
pub struct Document {
    /// The text.
    pub buffer: Buffer,
    /// Where it was last looked at. A window on screen keeps its own cursor;
    /// this is the one a window finds when it shows the document again, and
    /// what `:ls` reports when nothing is showing it.
    pub cursor: Cursor,
    /// What has been changed in it, for `u` and `<C-r>`: steps hold character
    /// indices into *this* text, so they can never be replayed onto another.
    pub history: History,
    /// The search that is up in it, matches and all.
    pub search: Search,
    /// What has been coloured in it, and the per-line state that needed.
    pub(crate) highlighter: LineStates,
    /// What a language server said about it, in the order it said it.
    pub diagnostics: Vec<Diagnostic>,
    /// The selection the last visual mode had, and where its cursor was.
    pub(crate) last_visual: Option<(Selection, Cursor)>,
    /// Where insert mode was last left.
    pub(crate) insert_again_at: Option<Cursor>,
    /// What this document is a list of, if it is one rather than a file. Such a
    /// document is shown but not edited: its lines *are* the list, and changing
    /// them would leave the list and the screen saying different things.
    pub list: Option<List>,
    /// Where each of its lines goes, in order.
    pub(crate) list_targets: Vec<(PathBuf, usize, usize)>,
    /// Where the marks are. A mark is a place in the text, so it stays with the
    /// text, and switching files takes it off the screen and brings it back.
    pub(crate) marks: Marks,
    /// The places this file has been looked at from: what `<C-o>` and `<C-i>`
    /// walk, and what `''` asks for.
    pub(crate) jumps: Jumps,
    /// Where the last jump started, which is what `''` and `` `` `` come back
    /// to — one place, so pressing one of them twice comes back again.
    pub(crate) jump_from: Option<Cursor>,
    /// The places edits happened: what `g;` and `g,` walk.
    pub(crate) changes: Changes,
    /// The settings `:setlocal` gave this file, kept with it so that showing it
    /// again puts them back.
    pub(crate) local_options: Vec<(crate::settings::Setting, Param)>,
}

impl Document {
    /// Whether this document has been changed since it was last written.
    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty()
    }

    /// How many edits this document has had. It only ever grows, which is what
    /// makes it a version number: a client that has sent the text at edit N has
    /// nothing to send until the count moves.
    pub fn edit_count(&self) -> u64 {
        self.history.edits()
    }

    /// What sits in a slot while the document itself is on screen.
    fn placeholder() -> Self {
        Self::new(Buffer::empty(), Language::Plain)
    }

    /// A document showing `buffer`, with nothing behind it.
    pub(crate) fn new(buffer: Buffer, language: Language) -> Self {
        let mut highlighter = LineStates::new(Language::Plain);
        highlighter.set_language(language);
        Self {
            buffer,
            cursor: Cursor::default(),
            history: History::new(),
            search: Search::new(),
            highlighter,
            diagnostics: Vec::new(),
            last_visual: None,
            insert_again_at: None,
            list: None,
            list_targets: Vec::new(),
            marks: Marks::default(),
            jumps: Jumps::default(),
            jump_from: None,
            changes: Changes::default(),
            local_options: Vec::new(),
        }
    }
}

/// A list this session has shown: what it was, and what it pointed at.
#[derive(Debug, Clone)]
pub(crate) struct ListSnapshot {
    /// Which kind of list it was.
    pub kind: List,
    /// The places it named, for `:cnext` to walk when it is shown again.
    pub places: Vec<(PathBuf, usize, usize)>,
}

/// Whether a row of a list is one the filter keeps: the row's number, and the
/// line it is written as.
type RowFilter = Box<dyn Fn(usize, &str) -> bool>;

/// What a document shown in a window of its own is a list of, rather than a file
/// to edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List {
    /// The places a language server named: references or symbols.
    Places,
    /// What a language server said about the file that is open.
    Diagnostics,
    /// Every setting and what it is set to, which is what `:set all` shows.
    Settings,
    /// What has been typed on the `:` line, which is what `q:` shows.
    Commands,
    /// What has been searched for, which is what `q/` shows.
    Searches,
}

impl List {
    /// Whether this list is *text* rather than a report.
    ///
    /// The two histories are what a person typed, so the window that shows them
    /// is a window like any other and the text in it can be edited — vim's
    /// command-line window: "you can now use commands to move around and edit the
    /// text in the window", and what is not run is "discarded when closing the
    /// window". The rest are derived from something else — a server's answer, the
    /// settings — and a list that can be edited is a list that can lie about what
    /// it says.
    pub fn is_editable(self) -> bool {
        matches!(self, List::Commands | List::Searches)
    }
}

/// A look at a document: where the cursor is in it, and where the window is
/// scrolled to.
///
/// The cursor is here and not in the document because it belongs to the *look*:
/// two windows on one file share every character and none of the position.
#[derive(Debug, Clone)]
pub struct Window {
    /// Which document this window shows.
    pub document: usize,
    /// Where the cursor is in it.
    pub cursor: Cursor,
    /// The first buffer line on screen.
    pub(crate) top_line: usize,
    /// The first visible column of each line.
    pub(crate) left_col: usize,
    /// The folds of this window. Vim keeps them per window — the same file can be
    /// folded one way in one window and another way in the next — and so does this.
    pub folds: Vec<Fold>,
}

impl Window {
    /// A window on `document`, at the top of it.
    pub(crate) fn new(document: usize) -> Self {
        Self {
            document,
            cursor: Cursor::default(),
            top_line: 0,
            left_col: 0,
            folds: Vec::new(),
        }
    }
}

/// Whether a fold is drawn closed, from the fold list and the level alone.
///
/// A free function because the *renderer* has the folds but not the editor, and the
/// rule must be the same in both: the drawing asks it per line, and the kernel asks it
/// for `zo`, `zc` and the cursor. The first version of this had the rule in the kernel
/// and a stale copy in the renderer, and the smoke showed a level that closed nothing.
pub(crate) fn fold_is_shut(
    folds: &[Fold],
    foldlevel: usize,
    minlines: usize,
    index: usize,
) -> bool {
    let Some(fold) = folds.get(index) else {
        return false;
    };
    let depth = folds
        .iter()
        .filter(|other| other.start < fold.start && other.end >= fold.end)
        .count()
        + 1;
    // `'foldminlines'`: "the number of screen lines above which a fold can be
    // displayed closed … With the default value of one a fold can only be closed
    // if it takes up two or more screen lines. Set to zero to be able to close
    // folds of just one screen line." It is about the *display*: the fold is
    // still closed, and `zo` still opens it — it is simply drawn open, which is
    // why this is the one place the rule lives rather than two.
    let lines = fold.end - fold.start + 1;
    (fold.closed || depth > foldlevel) && lines > minlines
}

/// A fold: a range of lines that can be drawn as its first one.
///
/// Vim's folds also have a *level*, which is what nesting is, and `zR`/`zM` walk
/// the levels. This has no levels: a fold is a range, and open or closed. Nesting
/// is on the roadmap, and what it would add is a level to compare instead of a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    /// The first line of the range.
    pub start: usize,
    /// The last line of it, which is always at or after `start`.
    pub end: usize,
    /// Whether it is drawn as one line.
    pub closed: bool,
}

/// How `:sort` compares two lines.
///
/// Vim spells the flags `:sort!` for the other way round, `:sort n` for numbers,
/// `:sort i` to ignore case and `:sort u` to keep one of each — and they can be
/// written together, which is why this is a struct and not a bool each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SortSpec {
    /// Sort the other way round.
    pub reverse: bool,
    /// Compare the numbers in the lines rather than the text.
    pub(crate) numeric: bool,
    /// Compare without case.
    pub(crate) ignore_case: bool,
    /// Keep one line of each, comparing the way the sort does.
    pub(crate) unique: bool,
}

impl SortSpec {
    /// What a line is compared by.
    fn key(&self, line: &str) -> String {
        let key = line.trim();
        if self.numeric {
            // A number anywhere in the line, which is what vim sorts by: the
            // first one, written the way it compares — so `9` comes before
            // `10` rather than after it.
            let mut digits = String::new();
            let mut started = false;
            for c in key.chars() {
                if c.is_ascii_digit() || (!started && c == '-') {
                    digits.push(c);
                    started = true;
                } else if started {
                    break;
                }
            }
            let value = digits.parse::<i64>().unwrap_or(0);
            return format!("{value:+020}");
        }
        if self.ignore_case {
            key.to_lowercase()
        } else {
            key.to_string()
        }
    }
}

/// How many matches one `a` answer may replace before it gives up. A pattern
/// that keeps matching what it writes is a pattern nobody wants an editor to
/// spend the afternoon on.
const MAX_CONFIRM_STEPS: usize = 10_000;

/// A `:s///c` in progress: one match at a time, each one asked about.
///
/// The matches are found one row at a time, and the search inside a row carries
/// on from where the last answer left it — which is what keeps a replacement
/// containing the pattern from being found again and again.
pub(crate) struct Confirming {
    spec: Substitute,
    pattern: regex::Regex,
    /// The rows to look at, in order.
    rows: Vec<usize>,
    /// Which of them is being looked at.
    row_at: usize,
    /// Where in that row the search carries on from, in characters.
    from: usize,
    /// What has been done so far, for the message at the end.
    count: usize,
    changed: Vec<usize>,
    /// Whether any question was asked at all. A pattern that never matched is
    /// "not found"; one the user said no to is simply zero substitutions.
    asked: bool,
    /// The match being asked about: its row, its start and end in the row, and
    /// the text it would be replaced with.
    asking: Option<(usize, usize, usize, String)>,
}

impl Confirming {
    /// The message that asks the question about the match in hand.
    pub(crate) fn prompt(&self) -> String {
        match &self.asking {
            Some((_, _, _, replacement)) => {
                format!("replace with {replacement} (y/n/a/q/l)?")
            }
            None => String::new(),
        }
    }
}

/// Which half of a text object a key asked for: `i` for the inside, `a` for the
/// inside and whatever surrounds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Object {
    /// `i` — the object itself.
    Inner,
    /// `a` — the object and the whitespace or brackets around it.
    Around,
}

/// One tab page: the windows it shows, how they tile it, and which of them is
/// up.
///
/// Documents are *not* here: a file is the same file whichever tab is looking at
/// it, which is what makes `:b other` in one tab change the file in every tab
/// that shows it.
pub struct Tab {
    windows: Vec<Window>,
    layout: Layout,
    current_window: usize,
}

impl Tab {
    fn new(windows: Vec<Window>, layout: Layout) -> Self {
        Self {
            windows,
            layout,
            current_window: 0,
        }
    }
}

/// Which way a split divides its rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    /// One above the other: `:sp`.
    Rows,
    /// Side by side: `:vs`.
    Columns,
}

/// How the windows tile the screen.
///
/// A tree rather than a list of rectangles, because that is what splitting *is*:
/// `:vs` then `:sp` divides one half of the screen and not the other. No node
/// carries a size — every frame divides the screen again, so a resize is free
/// and there is nothing to keep up to date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Layout {
    /// One window.
    Leaf(usize),
    /// Windows divided along an axis, in order.
    Split {
        /// Which way the division runs.
        axis: Axis,
        /// How big each child was asked to be, along the axis, in rows or
        /// columns — including the row a stacked window keeps for its status
        /// line. Zero means "no opinion", which is what a split made by `:sp`
        /// says: equal shares of whatever is left.
        sizes: Vec<u16>,
        /// The divisions, left to right or top to bottom.
        children: Vec<Layout>,
    },
}

impl Layout {
    /// Put `window` beside the leaf for `beside`, splitting along `axis`.
    ///
    /// A split that already divides along that axis takes the new window as
    /// another child of its own, so three `:sp`s make one division of the
    /// screen rather than a nest of them.
    fn split(&mut self, beside: usize, axis: Axis, window: usize) -> bool {
        match self {
            Layout::Leaf(here) if *here == beside => {
                *self = Layout::Split {
                    axis,
                    sizes: vec![0, 0],
                    children: vec![Layout::Leaf(beside), Layout::Leaf(window)],
                };
                true
            }
            Layout::Leaf(_) => false,
            Layout::Split {
                axis: here,
                sizes,
                children,
            } => {
                if *here == axis {
                    if let Some(at) = position(children, beside) {
                        children.insert(at + 1, Layout::Leaf(window));
                        sizes.insert(at + 1, 0);
                        return true;
                    }
                }
                children
                    .iter_mut()
                    .any(|child| child.split(beside, axis, window))
            }
        }
    }

    /// Take `window` out of the tree. A split left with one child becomes that
    /// child: the tree never keeps a division that divides nothing.
    fn remove(&mut self, window: usize) -> bool {
        match self {
            Layout::Leaf(here) => *here == window,
            Layout::Split {
                sizes, children, ..
            } => {
                if let Some(at) = position(children, window) {
                    children.remove(at);
                    sizes.remove(at);
                    if children.len() == 1 {
                        *self = children.remove(0);
                    }
                    return true;
                }
                children.iter_mut().any(|child| child.remove(window))
            }
        }
    }
}

/// The command a pending key sequence is waiting for its character on, if it is
/// a character find. The sequence is held in `pending`, which is also where a
/// half-typed keymap binding goes — the two never overlap, because a find is
/// only started when nothing is pending.
fn pending_find(pending: &str) -> Option<&'static str> {
    Some(match pending {
        "find_forward" => "find_forward",
        "find_backward" => "find_backward",
        "till_forward" => "till_forward",
        "till_backward" => "till_backward",
        _ => return None,
    })
}

/// A rectangle of the screen, in cells.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Rect {
    /// Column of the left edge.
    pub x: u16,
    /// Row of the top edge.
    pub y: u16,
    /// Width in columns.
    pub width: u16,
    /// Height in rows.
    pub height: u16,
}

/// Hand every leaf of `layout` its share of `rect`.
fn layout_rects(layout: &Layout, rect: Rect, out: &mut [Rect]) {
    match layout {
        Layout::Leaf(window) => {
            if let Some(slot) = out.get_mut(*window) {
                *slot = rect;
            }
        }
        Layout::Split {
            axis,
            sizes,
            children,
        } => {
            if children.is_empty() {
                return;
            }
            match axis {
                Axis::Columns => {
                    // One column between neighbours, for the separator, taken
                    // out of the shares rather than added to the rectangle.
                    let separators = children.len() as u16 - 1;
                    let usable = rect.width.saturating_sub(separators);
                    let shares = split_sizes(sizes, usable, 0);
                    let mut x = rect.x;
                    for (index, child) in children.iter().enumerate() {
                        let width = shares.get(index).copied().unwrap_or(0);
                        layout_rects(child, Rect { x, width, ..rect }, out);
                        x += width + 1;
                    }
                }
                Axis::Rows => {
                    // A window above another keeps the row between them for its
                    // status line, which is the line between them: the division
                    // costs no row of its own, so the shares are shares of the
                    // whole height.
                    let shares = split_sizes(sizes, rect.height, 1);
                    let mut y = rect.y;
                    for (index, child) in children.iter().enumerate() {
                        let height = shares.get(index).copied().unwrap_or(0);
                        layout_rects(child, Rect { y, height, ..rect }, out);
                        y += height;
                    }
                }
            }
        }
    }
}

/// Where in the tree the leaf for `window` is: the index of the child to
/// follow at each split on the way down.
fn layout_path(layout: &Layout, window: usize) -> Option<Vec<usize>> {
    match layout {
        Layout::Leaf(here) => (*here == window).then(Vec::new),
        Layout::Split { children, .. } => {
            for (index, child) in children.iter().enumerate() {
                if let Some(mut rest) = layout_path(child, window) {
                    let mut path = vec![index];
                    path.append(&mut rest);
                    return Some(path);
                }
            }
            None
        }
    }
}

/// Forget every size, so the windows are equal shares again.
fn clear_sizes(layout: &mut Layout) {
    if let Layout::Split {
        sizes, children, ..
    } = layout
    {
        sizes.fill(0);
        for child in children {
            clear_sizes(child);
        }
    }
}

/// Something only a language server can answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// What could be typed here.
    Completion {
        /// Where the cursor is.
        row: usize,
        /// And its column, in characters.
        col: usize,
    },
    /// What this is.
    Hover {
        /// Where the cursor is.
        row: usize,
        /// And its column.
        col: usize,
    },
    /// Where this is defined.
    Definition {
        /// Where the cursor is.
        row: usize,
        /// And its column.
        col: usize,
    },
    /// Everywhere this is used.
    References {
        /// Where the cursor is.
        row: usize,
        /// And its column.
        col: usize,
    },
    /// What the call being typed takes.
    Signature {
        /// Where the cursor is.
        row: usize,
        /// And its column.
        col: usize,
    },
    /// How this file should be laid out, according to a server.
    Format {
        /// Where the cursor is — formatting is about the file, but the question
        /// the kernel leaves carries a place like every other.
        row: usize,
        /// And its column.
        col: usize,
        /// The first line to format, when a range was named.
        from: Option<usize>,
        /// The last line, when one was.
        to: Option<usize>,
        /// Whether this formatting is part of a write: `formatonsave` asks for
        /// the file's layout as the file is being saved, and the answer writes
        /// what it laid out.
        save: bool,
    },
    /// What the whole project holds, for the word under the cursor.
    WorkspaceSymbols {
        /// Where the cursor is — the word there is the query, unless the
        /// command was given one.
        row: usize,
        /// And its column.
        col: usize,
        /// What to look for, when `:wsymbols {query}` named it.
        query: Option<String>,
    },
    /// What this file holds.
    Symbols {
        /// Where the cursor is — a document's symbols are not about a place, but
        /// the question the kernel leaves carries one like every other.
        row: usize,
        /// And its column.
        col: usize,
    },
}

impl Ask {
    /// Where it is about, as a row and a column in characters.
    pub fn position(&self) -> (usize, usize) {
        match *self {
            Ask::Completion { row, col }
            | Ask::Hover { row, col }
            | Ask::Definition { row, col }
            | Ask::References { row, col }
            | Ask::Symbols { row, col }
            | Ask::Signature { row, col }
            | Ask::WorkspaceSymbols { row, col, .. }
            | Ask::Format { row, col, .. } => (row, col),
        }
    }
}

/// The list of things that could be typed, and what is being completed.
///
/// The items come from wherever the session can find them — the words in the
/// buffer, or a language server — and nothing above this cares which: what a
/// person is doing is picking from a list.
pub struct Completion {
    /// Where in the buffer the completed word starts.
    pub start: usize,
    /// What the person had typed before the list was opened. `<C-e>` puts it
    /// back, which is the difference between "no thanks" and "not that one".
    pub typed: String,
    /// What is in the buffer now — the picked item, or what has been typed
    /// since. This is what a further keystroke filters by.
    pub prefix: String,
    /// Everything offered, before the prefix filtered it.
    pub all: Vec<CompletionItem>,
    /// What the prefix leaves, in the order it was offered.
    pub shown: Vec<CompletionItem>,
    /// Which of them is picked.
    pub selected: usize,
    /// Whether anything has been typed into the buffer yet. Picking the first
    /// item is what types it, the way vim's completion does: the list is a
    /// thing you look at *and* type from, not a menu you confirm.
    pub inserted: bool,
}

/// How many items are worth gathering before the list stops being a list.
const COMPLETION_LIMIT: usize = 200;

/// A separator being dragged with the pointer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Resizing {
    /// The window being made bigger or smaller.
    window: usize,
    /// Which way its size is measured.
    axis: Axis,
    /// What it was when the drag started, so the drag is a distance and not a
    /// sum of however many drag events arrived.
    base: u16,
    /// Where the pointer was when it started, along that axis.
    at: u16,
}

/// How much room a `:resize` asks for.
pub enum Resize {
    /// One row or column more: `:resize` with nothing after it.
    One,
    /// Make it this big.
    Set(u16),
    /// Add this many, taking them away when it is negative: `:resize +2`.
    By(i32),
}

/// How big each child of a split is, along its axis.
///
/// A child that asked for a size keeps it — plus `extra`, which is the row a
/// stacked window keeps for its status line and a side-by-side one does not
/// have. The ones with no opinion share what is left, and the last of them
/// takes the remainder, so a split always fills its rectangle whether or not
/// anyone has resized anything.
fn split_sizes(asked: &[u16], total: u16, extra: u16) -> Vec<u16> {
    let mut out = vec![0u16; asked.len()];
    let mut taken = 0u16;
    let mut unset = 0usize;
    for (index, size) in asked.iter().enumerate() {
        match *size {
            0 => unset += 1,
            size => {
                out[index] = size.saturating_add(extra).min(total);
                taken = taken.saturating_add(out[index]);
            }
        }
    }
    if unset == 0 {
        return out;
    }
    // What is left is shared equally, and the row or two that does not divide
    // goes to the earliest windows — which is what the division did before
    // anything could be resized, so nothing changes until someone says so.
    let left = total.saturating_sub(taken);
    let each = left / unset as u16;
    let remainder = left % unset as u16;
    let mut given = 0u16;
    for (index, size) in asked.iter().enumerate() {
        if *size != 0 {
            continue;
        }
        let share = if given < remainder { each + 1 } else { each };
        out[index] = share;
        given += 1;
    }
    out
}

/// Every window after the one that closed is a slot lower.
fn renumber(layout: &mut Layout, closed: usize) {
    match layout {
        Layout::Leaf(window) => {
            if *window > closed {
                *window -= 1;
            }
        }
        Layout::Split { children, .. } => {
            for child in children {
                renumber(child, closed);
            }
        }
    }
}

/// Where in `children` the leaf for `window` is, if it is a direct child.
fn position(children: &[Layout], window: usize) -> Option<usize> {
    children
        .iter()
        .position(|child| *child == Layout::Leaf(window))
}

/// The editor: the documents that are open, the windows looking at them, which
/// window is on screen, and everything that belongs to the session rather than
/// to a file — the register, the mode, the keymap, the command line, the
/// messages, and the script host.
pub struct Editor {
    /// Every open file. The one on screen is `documents[current_document()]`,
    /// whose fields are a placeholder while it is: see [`Document`].
    pub documents: Vec<Document>,
    /// The label of the item a language server should be asked about, when the
    /// list has moved to one.
    pub(crate) completion_resolve: Option<String>,
    /// The stops of a snippet that has been typed, as buffer ranges and what may
    /// be typed in each, and which one the cursor is on. `<Tab>` steps through
    /// them, `<C-n>` picks another of what a stop allows, and a session ends when
    /// there are no places left.
    pub(crate) snippet_stops: Vec<completion::Stop>,
    /// Which of the current stop's choices is the one in the text.
    pub(crate) snippet_choice: usize,
    /// Whether a mirror is being written right now, so that the edit it makes
    /// does not come back through the same path.
    pub(crate) snippet_mirrors_busy: bool,
    /// Which stop the cursor is on.
    pub(crate) snippet_step: usize,
    /// What is waiting for a mark's letter: `m`, `` ` `` or `'` has been typed
    /// and the letter has not. While this is set, no other key means anything.
    pub(crate) mark_prefix: Option<MarkJump>,
    /// Where the marks are, for the document on screen.
    pub(crate) marks: Marks,
    /// The jump list of the document on screen.
    pub(crate) jumps: Jumps,
    /// Where the last jump started, for the document on screen.
    pub(crate) jump_from: Option<Cursor>,
    /// The change list of the document on screen.
    pub(crate) changes: Changes,
    /// The selection the last visual mode had, and where its cursor was: what
    /// `gv` comes back to.
    pub(crate) last_visual: Option<(Selection, Cursor)>,
    /// Where insert mode was last left: what `gi` goes back to. (The *text* of
    /// the last insert session is a different field, and belongs to `.`.)
    pub(crate) insert_again_at: Option<Cursor>,
    /// A keyboard macro being recorded: `q{reg}` starts one and `q` ends it. The
    /// keys are kept as they are spelled (`<C-a>`, not a control byte), which is
    /// what makes playing one back the same as typing it.
    pub(crate) recording: Option<(char, String)>,
    /// The macros themselves, by register letter: what `@{letter}` plays.
    pub macros: [Option<String>; 26],
    /// The register `@` was last given, which is what `@@` plays.
    pub(crate) last_macro: Option<char>,
    /// What `q` or `@` is waiting for. It is *not* a second pending mechanism: it
    /// is what the keymap's own pending state means when the key that follows is
    /// one the table cannot name — a register letter, which is any letter.
    pub(crate) macro_ask: Option<MacroAsk>,
    /// Whether the keys being handled are a macro's, which is what keeps a
    /// replayed key out of the recording that is running.
    pub(crate) replaying: bool,
    /// How an operator's motion was forced to behave, and where the cursor was
    /// when it was forced: `v`, `V` and `<C-v>` after an operator — vim's `o_v`,
    /// `o_V` and `o_CTRL-V`. The cursor is one corner of a block; the other two
    /// only need the shape.
    pub(crate) forced_shape: Option<(ForcedShape, Cursor)>,
    /// Where the cursor was before the last edit, which is what tells a fold which
    /// way the cursor was going when it landed inside one.
    pub(crate) last_cursor_row: usize,
    /// Where `:mkview` writes and `:loadview` reads. `None` means the platform's
    /// config directory, and the tests point it somewhere of their own so that a
    /// test never writes into a person's config.
    pub(crate) view_dir: Option<PathBuf>,
    /// The folds as they were after each step of the history, so that an undo takes a
    /// fold back with the text it was on.
    ///
    /// A fold is not text: it is a range of *line numbers*, and the history holds
    /// character ranges. So this is a second stack, kept in step with the history's
    /// own count — truncated to it on every edit and pushed to — and `undo` and `redo`
    /// read it at the step they land on.
    pub(crate) fold_states: Vec<Vec<Fold>>,
    /// The folds of the window that is up. They belong to the *window* — the same
    /// file can be folded one way in one and another way in the next — so they are
    /// saved and loaded with the rest of a window's state.
    pub folds: Vec<Fold>,
    /// A blockwise insert: where the cursor is on each row of a rectangle, and
    /// which of them is the one being typed on.
    ///
    /// This is *not* per document, and deliberately: it lasts as long as the
    /// insert session, and a session cannot outlive a switch to another document.
    pub(crate) block_insert: Option<(Vec<Cursor>, usize)>,
    /// What the document on screen is a *list* of, if it is one rather than a
    /// file: a list is shown in a window of its own, and it is not for editing.
    pub list: Option<List>,
    /// Where each line of that list goes, in order — which is what `<CR>` reads.
    /// It belongs to the list rather than to the session, since two lists can be
    /// open at once.
    pub(crate) list_targets: Vec<(PathBuf, usize, usize)>,
    /// The lists this session has shown, oldest first, and which one is on screen.
    /// `:colder` and `:cnewer` walk it, which is what makes a narrowed list
    /// something a person can come back from.
    pub(crate) list_history: Vec<ListSnapshot>,
    /// Which of them the walk is on, counting from one, and zero when there are
    /// none.
    pub(crate) list_index: usize,
    /// The places the last question named — references or symbols — and where the
    /// walk through them is. They are across files, so this belongs to the
    /// session rather than to any one document.
    pub places: Vec<(PathBuf, usize, usize)>,
    /// Which place the walk is on, counting from one.
    pub place_index: usize,
    /// The mark a key has named and a command has not read yet: `` `a `` hands
    /// the letter to the command it runs, but `d`a` runs the *motion* by name,
    /// so the letter waits here the way a find's character does.
    pub(crate) last_mark: Option<(char, MarkJump)>,
    /// Every window, and how they tile the screen. The one on screen is
    /// `windows[current_window]`, whose fields are a placeholder while it is.
    pub windows: Vec<Window>,
    /// How the windows are laid out.
    pub(crate) layout: Layout,
    /// Which window that is.
    pub(crate) current_window: usize,
    /// Every tab page. The one on screen is `tabs[current_tab]`, whose state is
    /// the `windows`/`layout`/`current_window` above while it is.
    pub(crate) tabs: Vec<Tab>,
    /// Which tab that is.
    pub(crate) current_tab: usize,
    /// The document.
    pub buffer: Buffer,
    /// Where we are in it.
    pub cursor: Cursor,
    /// What has been changed, for `u` and `<C-r>`.
    pub history: History,
    /// What a key means right now.
    pub mode: Mode,
    /// Which part of the document is on screen.
    pub viewport: Viewport,
    /// The line being typed at the bottom: a `:` command or a `/` search.
    pub(crate) cmdline: String,
    /// Where the cursor is in that line, as a character index.
    pub(crate) cmdline_cursor: usize,
    /// `:` lines that have been run, oldest first, for `Up`.
    pub(crate) command_history: Vec<String>,
    /// `/` and `?` patterns, kept apart from the commands the way vim keeps
    /// them.
    pub(crate) search_history: Vec<String>,
    /// Which line of the history `Up`/`Down` is showing, if any.
    pub(crate) prompt_index: Option<usize>,
    /// What was being typed when `Up` was first pressed, so `Down` can bring it
    /// back.
    pub(crate) prompt_draft: String,
    /// What that line is for, which is what the prompt character says.
    pub(crate) prompt: Prompt,
    /// Transient status message shown on the bottom row.
    pub message: String,
    /// The built-in commands.
    pub(crate) registry: CommandRegistry,
    /// What keys do. Shared with the Lua host, so a script can remap at any
    /// time.
    pub(crate) keymap: Rc<RefCell<Keymap>>,
    /// Settings, shared with the Lua host for the same reason. These are the ones
    /// in force: the global value, unless the file that is open has a value of
    /// its own for that setting.
    pub options: Rc<RefCell<Options>>,
    /// What a setting is outside any file: the value `:setglobal` writes and a
    /// file with no value of its own falls back to.
    pub(crate) global_options: Options,
    /// The settings the file that is open has a value of its own for, in the
    /// order they were set.
    pub(crate) local_options: Vec<(crate::settings::Setting, Param)>,
    /// Keys of a partially-typed multi-key binding (`d` waiting for `d`).
    pub pending: String,
    /// Key presses in insert mode that may be the start of a mapping.
    ///
    /// They are held, not typed, until a key proves the sequence is not a
    /// mapping — at which point they are typed after all, in order. Nothing is
    /// lost and nothing arrives twice; a key that cannot start a mapping is
    /// typed straight away, so ordinary typing stays immediate.
    pub(crate) insert_pending: Vec<KeyMsg>,
    /// Digits typed before a command, which become its repeat count. `3x`,
    /// `2dd`, `5j`: the count is passed to the command as its first parameter,
    /// which is what the command `count()` already reads.
    pub(crate) pending_count: String,
    /// The operator a `d`, `y` or `c` started, waiting for its motion.
    pub operator: Option<Operator>,
    /// Which half of a text object is being asked for, while the key that names
    /// the object is still to come: `di` then `w`, `ca` then `"`.
    pub(crate) object: Option<Object>,
    /// How many times the pending operator repeats, so `2d3w` is six words.
    pub(crate) operator_count: usize,
    /// What the last yank or delete put aside, for `p` and `P`.
    pub register: Register,
    /// Every register a `"` can name: the letters, `"0`, the delete history,
    /// the black hole and the clipboard.
    pub(crate) registers: Registers,
    /// A `"` that has been typed and the register it names, if the name has
    /// come: `"ayy` is `Prefix::Named('a')` by the time `yy` runs.
    pub(crate) register_prefix: Prefix,
    /// The character a find is looking for, and which way it looks. `;` and `,`
    /// use it again, and so do `df,` and `.` after it.
    pub(crate) last_find: Option<(char, crate::command::Find)>,
    /// The last `:s` line that was run, for `&` to run again on the line the
    /// cursor is on.
    pub(crate) last_substitute: Option<Substitute>,
    /// A `:s///c` waiting for an answer about a match.
    pub(crate) confirm: Option<Confirming>,
    /// A separator being dragged, if one is.
    pub(crate) resizing: Option<Resizing>,
    /// The completion popup, while it is up.
    pub completion: Option<Completion>,
    /// A question for a language server, if one is waiting to be asked.
    ///
    /// The kernel does not know about servers: it leaves the question here and
    /// carries on with what it can answer itself, and a session that has a
    /// server takes the question and asks it.
    pub(crate) lsp_ask: Option<Ask>,
    /// The command that is running, if one is, and what its output is for.
    ///
    /// The kernel does not run anything — see `jobs.rs` — so this is a command
    /// that has been asked for, and the answer that will come back to it.
    pub(crate) job: Option<jobs::Job>,
    /// Set by `:kill`, taken by the session that owns the process.
    pub(crate) job_kill: bool,
    /// The last command that changed something, with the arguments it was run
    /// with: `.` runs it again.
    pub(crate) last_command: Option<(String, Vec<Param>)>,
    /// The text the insert session of that change typed, in the order it was
    /// typed, with a backspace and a delete as tokens of their own. `.` types it
    /// again after running the command.
    pub(crate) last_insert: Option<String>,
    /// What the insert session that is open has typed so far. `Some` only while
    /// one is being recorded, which is from entering insert mode by a command to
    /// leaving it.
    insert_log: Option<String>,
    /// The visual selection while [`Mode::Visual`] is up: where it started, and
    /// whether it covers characters or whole lines. The cursor is its moving
    /// end. A mouse drag makes the same thing, so a selection is a selection
    /// however it was made.
    pub(crate) selection: Option<Selection>,
    /// Whether the pointer has dragged since it was pressed, which is what tells
    /// a click from a drag.
    pub(crate) mouse_dragged: bool,
    /// How many clicks in a row the press that is in progress was: what it
    /// selected (a word, a line, a block) and what a drag from it extends by.
    pointer_clicks: u8,
    /// Text on its way to the system clipboard: a command fills this in, the
    /// daemon turns it into a message, the frontend writes it where text goes.
    /// Nothing in the kernel can reach a clipboard, and nothing here pretends
    /// to.
    pub clipboard: Option<String>,
    /// The search that is up, if one has been run.
    pub search: Search,
    /// The search being typed, when `'incsearch'` is showing one.
    incsearch: Option<IncSearch>,
    /// Whether the highlighting of matches has been turned off by
    /// `:nohlsearch`, which is not the same as `'hlsearch'` being off: it is
    /// forgotten as soon as a search is used again.
    nohlsearch: bool,
    /// What a language server said about the document on screen.
    pub diagnostics: Vec<Diagnostic>,
    /// The syntax colours, and the per-line state they need: what a line starts
    /// in depends on every line above it, so the answers are cached.
    pub(crate) highlighter: LineStates,
    /// Set by a command that wants the session to end; the daemon turns it
    /// into an [`inkstone_rpc::ServerMsg::Quit`].
    pub should_quit: bool,
    /// Set whenever the screen may have changed; the daemon uses it to decide
    /// whether to emit a redraw.
    pub redraw: bool,
    /// The script host.
    pub lua: LuaHost,
}

impl Editor {
    /// Build an editor, optionally sourcing `init_path` as Lua.
    ///
    /// A broken `init.lua` is reported on the status line rather than being
    /// fatal: a config typo must not stop the editor from opening.
    pub fn new(init_path: Option<&Path>) -> Result<Self, CoreError> {
        let registry = CommandRegistry::with_builtins();
        let keymap = Rc::new(RefCell::new(Keymap::with_defaults()));
        let options = Rc::new(RefCell::new(Options::default()));
        let lua = LuaHost::new(&registry, Rc::clone(&keymap), Rc::clone(&options))?;
        // The settings outside any file start as the defaults, which is what the
        // live ones are until a `:setlocal` gives the file that is open a value
        // of its own.
        let global_options = (*options.borrow()).clone();
        let mut editor = Self {
            global_options,
            local_options: Vec::new(),
            view_dir: None,
            fold_states: Vec::new(),
            last_cursor_row: 0,
            folds: Vec::new(),
            recording: None,
            macros: Default::default(),
            last_macro: None,
            macro_ask: None,
            replaying: false,
            forced_shape: None,
            block_insert: None,
            documents: vec![Document::placeholder()],
            last_visual: None,
            insert_again_at: None,
            list_history: Vec::new(),
            list_index: 0,
            completion_resolve: None,
            snippet_stops: Vec::new(),
            snippet_step: 0,
            snippet_choice: 0,
            snippet_mirrors_busy: false,
            mark_prefix: None,
            marks: Marks::default(),
            jumps: Jumps::default(),
            jump_from: None,
            changes: Changes::default(),
            list: None,
            list_targets: Vec::new(),
            places: Vec::new(),
            place_index: 0,
            last_mark: None,
            windows: vec![Window::new(0)],
            layout: Layout::Leaf(0),
            current_window: 0,
            tabs: vec![Tab::new(vec![Window::new(0)], Layout::Leaf(0))],
            current_tab: 0,
            buffer: Buffer::empty(),
            cursor: Cursor::default(),
            history: History::new(),
            mode: Mode::Normal,
            viewport: Viewport::new(80, 24),
            cmdline: String::new(),
            cmdline_cursor: 0,
            command_history: Vec::new(),
            search_history: Vec::new(),
            prompt_index: None,
            prompt_draft: String::new(),
            prompt: Prompt::Command,
            message: String::new(),
            registry,
            keymap,
            options,
            pending: String::new(),
            insert_pending: Vec::new(),
            pending_count: String::new(),
            operator: None,
            object: None,
            operator_count: 1,
            register: Register::empty(),
            registers: Registers::new(),
            register_prefix: Prefix::None,
            last_find: None,
            last_substitute: None,
            confirm: None,
            resizing: None,
            completion: None,
            lsp_ask: None,
            job: None,
            job_kill: false,
            last_command: None,
            last_insert: None,
            insert_log: None,
            selection: None,
            mouse_dragged: false,
            pointer_clicks: 1,
            clipboard: None,
            search: Search::new(),
            incsearch: None,
            nohlsearch: false,
            highlighter: LineStates::new(Language::Plain),
            diagnostics: Vec::new(),
            should_quit: false,
            redraw: true,
            lua,
        };
        if let Some(path) = init_path {
            if path.exists() {
                if let Err(e) = editor.lua.load_init(path) {
                    editor.message = format!("init.lua: {e}");
                }
            }
        }
        Ok(editor)
    }

    /// A default editor with no Lua config, for tests.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::new(None).expect("editor construction cannot fail without a config file")
    }

    // -- state transitions --------------------------------------------------
}

fn byte_index(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(index, _)| index)
}

/// The digit a key contributes to a count, if it contributes one.
///
/// `0` only counts once a count is running: on its own it is still vim's
/// `move_line_start`, which is what makes `d0` delete to the start of the line.
fn count_digit(key: &KeyMsg, running: bool) -> Option<char> {
    match key.code {
        KeyCode::Char(c)
            if !key.is_ctrl() && !key.is_alt() && c.is_ascii_digit() && (c != '0' || running) =>
        {
            Some(c)
        }
        _ => None,
    }
}

/// A count as a command's first parameter. A count of one is not sent at all,
/// so a command that never sees a count behaves exactly as it did before.
fn count_params(count: usize) -> Vec<Param> {
    if count > 1 {
        vec![Param::Int(count as i64)]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests;
