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

use inkstone_rpc::{KeyCode, KeyMsg, Param, PointerKind, PointerMsg};

use crate::buffer::{Buffer, BufferError};
use crate::changes::Changes;
use crate::command::{self, binding_motion, CommandError, CommandRegistry};
use crate::completion::{self, Item as CompletionItem};
use crate::cursor::Cursor;
use crate::diagnostic::{order as diagnostic_order, Diagnostic};
use crate::highlight::{Language, LineStates};
use crate::history::{Change, History};
use crate::jumps::Jumps;
use crate::keymap::{encode_key, Binding, Keymap, Lookup};
use crate::lua::LuaHost;
use crate::marks::{MarkJump, Marks};
use crate::mode::{Mode, VisualKind};
use crate::register::{is_register_name, Kind, Prefix, Register, Registers};
use crate::render::{compose, gutter_width, position_at, status_line, Composed, View};
use crate::screen::{scroll_to_cursor, Viewport};
use crate::search::{Direction, Search};
use crate::settings::{Options, Setting};
use crate::substitute::{Address, Substitute};
use crate::CoreError;

/// Guard against a Lua command calling itself forever.
pub const MAX_COMMAND_DEPTH: usize = 100;

/// How many lines one notch of the wheel scrolls the view.
pub const MOUSE_SCROLL_LINES: i64 = 3;

/// How many digits a count may have. Long enough for any count a person types,
/// short enough that a leaned-on key cannot overflow one.
pub const MAX_COUNT_DIGITS: usize = 9;

/// What the line at the bottom of the screen is asking for.
///
/// One mode for all three, because a `:` command and a `/` search are the same
/// thing to type into: a line with a cursor, a backspace and an escape. Only the
/// first character differs, and what running it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
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
    pub fn direction(self) -> Option<Direction> {
        match self {
            Prompt::Command => None,
            Prompt::SearchForward => Some(Direction::Forward),
            Prompt::SearchBackward => Some(Direction::Backward),
        }
    }
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
pub struct Selection {
    /// Where the selection started.
    pub anchor: Cursor,
    /// Whether it covers characters or whole lines.
    pub kind: VisualKind,
    /// A blockwise selection whose right edge is the end of each line rather than
    /// a column: `<C-v>$` makes a *ragged* block, which is what `d` on it means —
    /// "from here to the end of every line", not "this many columns".
    pub to_end: bool,
}

/// The operator a `d`, `y` or `c` started, waiting for the motion it works on.
///
/// What a macro key is waiting for: `q` for a register to record into (or to stop
/// recording), and `@` for one to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroAsk {
    /// `q` — the next key names the register to record into, or ends the recording.
    Record,
    /// `@` — the next key names the register to play.
    Play,
}

/// How an operator's motion was forced to behave, by `v`, `V` or `<C-v>` typed
/// after the operator — vim's `o_v`, `o_V` and `o_CTRL-V`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForcedShape {
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
    pub fn visual_command(self) -> &'static str {
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
    pub fn line_command(self) -> &'static str {
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
    pub highlighter: LineStates,
    /// What a language server said about it, in the order it said it.
    pub diagnostics: Vec<Diagnostic>,
    /// The selection the last visual mode had, and where its cursor was.
    pub last_visual: Option<(Selection, Cursor)>,
    /// Where insert mode was last left.
    pub insert_again_at: Option<Cursor>,
    /// What this document is a list of, if it is one rather than a file. Such a
    /// document is shown but not edited: its lines *are* the list, and changing
    /// them would leave the list and the screen saying different things.
    pub list: Option<List>,
    /// Where each of its lines goes, in order.
    pub list_targets: Vec<(PathBuf, usize, usize)>,
    /// Where the marks are. A mark is a place in the text, so it stays with the
    /// text, and switching files takes it off the screen and brings it back.
    pub marks: Marks,
    /// The places this file has been looked at from: what `<C-o>` and `<C-i>`
    /// walk, and what `''` asks for.
    pub jumps: Jumps,
    /// Where the last jump started, which is what `''` and `` `` `` come back
    /// to — one place, so pressing one of them twice comes back again.
    pub jump_from: Option<Cursor>,
    /// The places edits happened: what `g;` and `g,` walk.
    pub changes: Changes,
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
        }
    }
}

/// A list this session has shown: what it was, and what it pointed at.
#[derive(Debug, Clone)]
pub struct ListSnapshot {
    /// Which kind of list it was.
    pub kind: List,
    /// The places it named, for `:cnext` to walk when it is shown again.
    pub places: Vec<(PathBuf, usize, usize)>,
    /// Where each of its lines went.
    pub targets: Vec<(PathBuf, usize, usize)>,
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
    pub top_line: usize,
    /// The first visible column of each line.
    pub left_col: usize,
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
pub fn fold_is_shut(folds: &[Fold], foldlevel: usize, index: usize) -> bool {
    let Some(fold) = folds.get(index) else {
        return false;
    };
    let depth = folds
        .iter()
        .filter(|other| other.start < fold.start && other.end >= fold.end)
        .count()
        + 1;
    fold.closed || depth > foldlevel
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
pub struct SortSpec {
    /// Sort the other way round.
    pub reverse: bool,
    /// Compare the numbers in the lines rather than the text.
    pub numeric: bool,
    /// Compare without case.
    pub ignore_case: bool,
    /// Keep one line of each, comparing the way the sort does.
    pub unique: bool,
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
pub struct Confirming {
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
    pub fn prompt(&self) -> String {
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
pub enum Object {
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
pub enum Axis {
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
pub enum Layout {
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
pub struct Rect {
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
pub struct Resizing {
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
    pub completion_resolve: Option<String>,
    /// The stops of a snippet that has been typed, as buffer ranges and what may
    /// be typed in each, and which one the cursor is on. `<Tab>` steps through
    /// them, `<C-n>` picks another of what a stop allows, and a session ends when
    /// there are no places left.
    pub snippet_stops: Vec<completion::Stop>,
    /// Which of the current stop's choices is the one in the text.
    pub snippet_choice: usize,
    /// Whether a mirror is being written right now, so that the edit it makes
    /// does not come back through the same path.
    pub snippet_mirrors_busy: bool,
    /// Which stop the cursor is on.
    pub snippet_step: usize,
    /// What is waiting for a mark's letter: `m`, `` ` `` or `'` has been typed
    /// and the letter has not. While this is set, no other key means anything.
    pub mark_prefix: Option<MarkJump>,
    /// Where the marks are, for the document on screen.
    pub marks: Marks,
    /// The jump list of the document on screen.
    pub jumps: Jumps,
    /// Where the last jump started, for the document on screen.
    pub jump_from: Option<Cursor>,
    /// The change list of the document on screen.
    pub changes: Changes,
    /// The selection the last visual mode had, and where its cursor was: what
    /// `gv` comes back to.
    pub last_visual: Option<(Selection, Cursor)>,
    /// Where insert mode was last left: what `gi` goes back to. (The *text* of
    /// the last insert session is a different field, and belongs to `.`.)
    pub insert_again_at: Option<Cursor>,
    /// A keyboard macro being recorded: `q{reg}` starts one and `q` ends it. The
    /// keys are kept as they are spelled (`<C-a>`, not a control byte), which is
    /// what makes playing one back the same as typing it.
    pub recording: Option<(char, String)>,
    /// The macros themselves, by register letter: what `@{letter}` plays.
    pub macros: [Option<String>; 26],
    /// The register `@` was last given, which is what `@@` plays.
    pub last_macro: Option<char>,
    /// What `q` or `@` is waiting for. It is *not* a second pending mechanism: it
    /// is what the keymap's own pending state means when the key that follows is
    /// one the table cannot name — a register letter, which is any letter.
    pub macro_ask: Option<MacroAsk>,
    /// Whether the keys being handled are a macro's, which is what keeps a
    /// replayed key out of the recording that is running.
    pub replaying: bool,
    /// How an operator's motion was forced to behave, and where the cursor was
    /// when it was forced: `v`, `V` and `<C-v>` after an operator — vim's `o_v`,
    /// `o_V` and `o_CTRL-V`. The cursor is one corner of a block; the other two
    /// only need the shape.
    pub forced_shape: Option<(ForcedShape, Cursor)>,
    /// Where the cursor was before the last edit, which is what tells a fold which
    /// way the cursor was going when it landed inside one.
    pub last_cursor_row: usize,
    /// Where `:mkview` writes and `:loadview` reads. `None` means the platform's
    /// config directory, and the tests point it somewhere of their own so that a
    /// test never writes into a person's config.
    pub view_dir: Option<PathBuf>,
    /// The folds as they were after each step of the history, so that an undo takes a
    /// fold back with the text it was on.
    ///
    /// A fold is not text: it is a range of *line numbers*, and the history holds
    /// character ranges. So this is a second stack, kept in step with the history's
    /// own count — truncated to it on every edit and pushed to — and `undo` and `redo`
    /// read it at the step they land on.
    pub fold_states: Vec<Vec<Fold>>,
    /// The folds of the window that is up. They belong to the *window* — the same
    /// file can be folded one way in one and another way in the next — so they are
    /// saved and loaded with the rest of a window's state.
    pub folds: Vec<Fold>,
    /// A blockwise insert: where the cursor is on each row of a rectangle, and
    /// which of them is the one being typed on.
    ///
    /// This is *not* per document, and deliberately: it lasts as long as the
    /// insert session, and a session cannot outlive a switch to another document.
    pub block_insert: Option<(Vec<Cursor>, usize)>,
    /// What the document on screen is a *list* of, if it is one rather than a
    /// file: a list is shown in a window of its own, and it is not for editing.
    pub list: Option<List>,
    /// Where each line of that list goes, in order — which is what `<CR>` reads.
    /// It belongs to the list rather than to the session, since two lists can be
    /// open at once.
    pub list_targets: Vec<(PathBuf, usize, usize)>,
    /// The lists this session has shown, oldest first, and which one is on screen.
    /// `:colder` and `:cnewer` walk it, which is what makes a narrowed list
    /// something a person can come back from.
    pub list_history: Vec<ListSnapshot>,
    /// Which of them the walk is on, counting from one, and zero when there are
    /// none.
    pub list_index: usize,
    /// The places the last question named — references or symbols — and where the
    /// walk through them is. They are across files, so this belongs to the
    /// session rather than to any one document.
    pub places: Vec<(PathBuf, usize, usize)>,
    /// Which place the walk is on, counting from one.
    pub place_index: usize,
    /// The mark a key has named and a command has not read yet: `` `a `` hands
    /// the letter to the command it runs, but `d`a` runs the *motion* by name,
    /// so the letter waits here the way a find's character does.
    pub last_mark: Option<(char, MarkJump)>,
    /// Every window, and how they tile the screen. The one on screen is
    /// `windows[current_window]`, whose fields are a placeholder while it is.
    pub windows: Vec<Window>,
    /// How the windows are laid out.
    pub layout: Layout,
    /// Which window that is.
    pub current_window: usize,
    /// Every tab page. The one on screen is `tabs[current_tab]`, whose state is
    /// the `windows`/`layout`/`current_window` above while it is.
    pub tabs: Vec<Tab>,
    /// Which tab that is.
    pub current_tab: usize,
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
    pub cmdline: String,
    /// Where the cursor is in that line, as a character index.
    pub cmdline_cursor: usize,
    /// `:` lines that have been run, oldest first, for `Up`.
    pub command_history: Vec<String>,
    /// `/` and `?` patterns, kept apart from the commands the way vim keeps
    /// them.
    pub search_history: Vec<String>,
    /// Which line of the history `Up`/`Down` is showing, if any.
    pub prompt_index: Option<usize>,
    /// What was being typed when `Up` was first pressed, so `Down` can bring it
    /// back.
    pub prompt_draft: String,
    /// What that line is for, which is what the prompt character says.
    pub prompt: Prompt,
    /// Transient status message shown on the bottom row.
    pub message: String,
    /// The built-in commands.
    pub registry: CommandRegistry,
    /// What keys do. Shared with the Lua host, so a script can remap at any
    /// time.
    pub keymap: Rc<RefCell<Keymap>>,
    /// Settings, shared with the Lua host for the same reason.
    pub options: Rc<RefCell<Options>>,
    /// Keys of a partially-typed multi-key binding (`d` waiting for `d`).
    pub pending: String,
    /// Key presses in insert mode that may be the start of a mapping.
    ///
    /// They are held, not typed, until a key proves the sequence is not a
    /// mapping — at which point they are typed after all, in order. Nothing is
    /// lost and nothing arrives twice; a key that cannot start a mapping is
    /// typed straight away, so ordinary typing stays immediate.
    pub insert_pending: Vec<KeyMsg>,
    /// Digits typed before a command, which become its repeat count. `3x`,
    /// `2dd`, `5j`: the count is passed to the command as its first parameter,
    /// which is what the command `count()` already reads.
    pub pending_count: String,
    /// The operator a `d`, `y` or `c` started, waiting for its motion.
    pub operator: Option<Operator>,
    /// Which half of a text object is being asked for, while the key that names
    /// the object is still to come: `di` then `w`, `ca` then `"`.
    pub object: Option<Object>,
    /// How many times the pending operator repeats, so `2d3w` is six words.
    pub operator_count: usize,
    /// What the last yank or delete put aside, for `p` and `P`.
    pub register: Register,
    /// Every register a `"` can name: the letters, `"0`, the delete history,
    /// the black hole and the clipboard.
    pub registers: Registers,
    /// A `"` that has been typed and the register it names, if the name has
    /// come: `"ayy` is `Prefix::Named('a')` by the time `yy` runs.
    pub register_prefix: Prefix,
    /// The character a find is looking for, and which way it looks. `;` and `,`
    /// use it again, and so do `df,` and `.` after it.
    pub last_find: Option<(char, crate::command::Find)>,
    /// The last `:s` line that was run, for `&` to run again on the line the
    /// cursor is on.
    pub last_substitute: Option<Substitute>,
    /// A `:s///c` waiting for an answer about a match.
    pub confirm: Option<Confirming>,
    /// A separator being dragged, if one is.
    pub resizing: Option<Resizing>,
    /// The completion popup, while it is up.
    pub completion: Option<Completion>,
    /// A question for a language server, if one is waiting to be asked.
    ///
    /// The kernel does not know about servers: it leaves the question here and
    /// carries on with what it can answer itself, and a session that has a
    /// server takes the question and asks it.
    pub lsp_ask: Option<Ask>,
    /// The last command that changed something, with the arguments it was run
    /// with: `.` runs it again.
    pub last_command: Option<(String, Vec<Param>)>,
    /// The text the insert session of that change typed, in the order it was
    /// typed, with a backspace and a delete as tokens of their own. `.` types it
    /// again after running the command.
    pub last_insert: Option<String>,
    /// What the insert session that is open has typed so far. `Some` only while
    /// one is being recorded, which is from entering insert mode by a command to
    /// leaving it.
    insert_log: Option<String>,
    /// The visual selection while [`Mode::Visual`] is up: where it started, and
    /// whether it covers characters or whole lines. The cursor is its moving
    /// end. A mouse drag makes the same thing, so a selection is a selection
    /// however it was made.
    pub selection: Option<Selection>,
    /// Whether the pointer has dragged since it was pressed, which is what tells
    /// a click from a drag.
    pub mouse_dragged: bool,
    /// Text on its way to the system clipboard: a command fills this in, the
    /// daemon turns it into a message, the frontend writes it where text goes.
    /// Nothing in the kernel can reach a clipboard, and nothing here pretends
    /// to.
    pub clipboard: Option<String>,
    /// The search that is up, if one has been run.
    pub search: Search,
    /// The search being typed, when `'incsearch'` is showing one.
    incsearch: Option<IncSearch>,
    /// What a language server said about the document on screen.
    pub diagnostics: Vec<Diagnostic>,
    /// The syntax colours, and the per-line state they need: what a line starts
    /// in depends on every line above it, so the answers are cached.
    pub highlighter: LineStates,
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
        let mut editor = Self {
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
            last_command: None,
            last_insert: None,
            insert_log: None,
            selection: None,
            mouse_dragged: false,
            clipboard: None,
            search: Search::new(),
            incsearch: None,
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
    pub fn for_tests() -> Self {
        Self::new(None).expect("editor construction cannot fail without a config file")
    }

    // -- state transitions --------------------------------------------------

    /// Feed one key press through the mode state machine.
    pub fn handle_key(&mut self, key: KeyMsg) {
        // A message lives until the next key *outside* command mode; the key
        // that finishes a `:` command must not erase what it just reported. A
        // `:s///c` is the other exception: its question *is* the message, and
        // the keys that answer it must not erase it before they are read.
        if self.mode != Mode::Command && self.confirm.is_none() {
            self.message.clear();
        }
        // A macro being recorded keeps every key typed *into* it, but not the keys
        // this editor is replaying: a macro that contained itself would never end.
        if !self.replaying {
            if let Some((_, keys)) = self.recording.as_mut() {
                keys.push_str(&encode_key(&key));
            }
        }
        match self.mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Visual => self.handle_visual(key),
            Mode::Insert => {
                self.handle_insert(key.clone());
                self.replay_block_insert(key);
            }
            Mode::Command => self.handle_command_line(key),
        }
        // "When in Insert mode, the cursor line is never folded. That allows you to see
        // what you type!" — so entering insert opens the fold the cursor is on.
        if self.mode == Mode::Insert {
            self.open_fold_under_cursor();
        }
        self.redraw = true;
    }

    /// Open the fold the cursor's line is in, if a closed one is: what insert mode
    /// needs, and what a person asking to type on a folded line means.
    pub fn open_fold_under_cursor(&mut self) {
        let row = self.cursor.row;
        let Some(index) = (0..self.folds.len()).find(|index| {
            let fold = self.folds[*index];
            self.fold_is_shut(*index) && row >= fold.start && row <= fold.end
        }) else {
            return;
        };
        self.set_fold_closed_at(index, false);
    }

    /// The terminal changed size.
    pub fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.viewport.resize(cols, rows);
        self.reposition();
        self.redraw = true;
    }

    /// The pointer did something, in screen cells.
    ///
    /// A press puts the cursor where it was pointed and starts a selection; a
    /// drag extends it; a release that never dragged collapses it again, which
    /// is what makes a click a click. The wheel moves the view and leaves the
    /// cursor where the user left it.
    pub fn handle_pointer(&mut self, msg: &PointerMsg) {
        match msg.kind {
            PointerKind::Press => {
                // A press on the line between two windows moves that line, and
                // does not start a selection: the separator is not text.
                if let Some(resizing) = self.separator_at(msg.x, msg.y) {
                    self.resizing = Some(resizing);
                    self.selection = None;
                    self.mode = Mode::Normal;
                    self.clear_pending();
                    self.redraw = true;
                    return;
                }
                self.resizing = None;
                // Anything that was selected is done with: a click starts over.
                self.selection = None;
                self.mode = Mode::Normal;
                self.clear_pending();
                if let Some((window, at)) = self.position_at_screen(msg.x, msg.y) {
                    // A press in another window goes to that window first, the
                    // way a click in vim does.
                    self.switch_to_window(window);
                    self.cursor = at;
                    self.clamp_cursor();
                    self.selection = Some(Selection {
                        anchor: self.cursor,
                        kind: VisualKind::Charwise,
                        to_end: false,
                    });
                    self.mode = Mode::Visual;
                    self.mouse_dragged = false;
                    self.after_edit();
                }
            }
            PointerKind::Drag => {
                if let Some(resizing) = self.resizing {
                    // The size is the one it started at, moved by how far the
                    // pointer has come — not by how many events arrived.
                    let delta = match resizing.axis {
                        Axis::Columns => msg.x as i32 - resizing.at as i32,
                        Axis::Rows => msg.y as i32 - resizing.at as i32,
                    };
                    let size = (resizing.base as i32 + delta).max(1);
                    self.resize_window_at(
                        resizing.window,
                        resizing.axis,
                        Resize::Set(size.min(u16::MAX as i32) as u16),
                    );
                    self.redraw = true;
                    return;
                }
                if self.selection.is_some() {
                    if let Some((window, at)) = self.position_at_screen(msg.x, msg.y) {
                        // A drag that leaves this window is still this window's
                        // selection: the anchor is here.
                        if window == self.current_window {
                            self.mouse_dragged = true;
                            self.cursor = at;
                            self.clamp_cursor();
                            self.after_edit();
                        }
                    }
                }
            }
            PointerKind::Release => {
                self.resizing = None;
                if self.selection.is_some() && !self.mouse_dragged {
                    // A press and a release with no drag between them is a
                    // click: the cursor stays, the selection goes.
                    self.selection = None;
                    self.mode = Mode::Normal;
                }
                self.mouse_dragged = false;
                self.after_edit();
            }
            PointerKind::ScrollUp => self.scroll_view(-MOUSE_SCROLL_LINES),
            PointerKind::ScrollDown => self.scroll_view(MOUSE_SCROLL_LINES),
        }
        self.redraw = true;
    }

    /// Put what the editor is holding on its way to the clipboard: the
    /// selection if one is up, the register otherwise.
    pub fn request_copy(&mut self) {
        self.clipboard = Some(self.text_for_clipboard());
    }

    /// What a copy would send: the selection, or what the register holds.
    ///
    /// Bytes exactly as the buffer has them, terminators included, so a linewise
    /// selection pastes as whole lines and a CRLF file copies CRLF.
    pub fn text_for_clipboard(&self) -> String {
        match self.visual_span() {
            Some((start, end)) => self.buffer.slice_text(start, end),
            None => self.register.text().to_string(),
        }
    }

    /// The buffer position a screen cell shows, if it shows text at all.
    /// The window and the position a screen cell shows. A click in a window
    /// that is not the current one has to go there first, which the caller does
    /// with the window that comes back.
    fn position_at_screen(&self, x: u16, y: u16) -> Option<(usize, Cursor)> {
        position_at(&self.view(), x, y)
    }

    /// Move what is on screen, without moving the cursor.
    fn scroll_view(&mut self, lines: i64) {
        let last = self.buffer.line_count().saturating_sub(1) as i64;
        self.viewport.top_line = (self.viewport.top_line as i64 + lines).clamp(0, last) as usize;
    }

    /// The view of the editor that drawing and hit-testing share.
    fn view(&self) -> View<'_> {
        let options = self.options.borrow().clone();
        View {
            buffer: &self.buffer,
            cursor: self.cursor,
            mode: self.mode,
            selection: self.selection.and_then(|_| self.visual_span()),
            matches: self.search.matches(),
            current_match: self.search.match_at(self.cursor.char_idx(&self.buffer)),
            incsearch: self
                .incsearch
                .as_ref()
                .is_some_and(|preview| preview.showing),
            highlighter: options.highlight.then_some(&self.highlighter),
            viewport: self.viewport,
            block: self.visual_block(),
            folds: &self.folds,
            foldlevel: self.options.borrow().foldlevel.0,
            rects: self.window_rects(),
            documents: &self.documents,
            windows: &self.windows,
            current_window: self.current_window,
            completion: self.completion.as_ref(),
            diagnostics: &self.diagnostics,
            dirty: self.is_dirty(),
            tabstop: options.tabstop,
            number: options.number,
            status: self.status(),
            cmdline: self
                .in_prompt()
                .then(|| format!("{}{}", self.prompt.spelling(), self.cmdline)),
            cmdline_cursor: self.in_prompt().then_some(self.cmdline_cursor),
            message: self.message.clone(),
        }
    }

    /// A programmatic command, as sent over RPC.
    pub fn handle_command(&mut self, name: &str, params: &[Param]) {
        self.run_command(name, params);
        self.redraw = true;
    }

    /// Replace the buffer with the contents of `path`.
    pub fn load_file(&mut self, path: &Path) -> Result<(), BufferError> {
        let buffer = Buffer::from_path(path)?;
        self.buffer = buffer;
        self.highlighter.set_language(Language::from_path(path));
        self.cursor = Cursor::default();
        // The history belongs to the document that was open: its steps hold
        // character indices into it, so they cannot be replayed onto this one.
        self.history.reset();
        self.viewport.top_line = 0;
        self.viewport.left_col = 0;
        self.mode = Mode::Normal;
        self.cmdline.clear();
        self.cmdline_cursor = 0;
        self.forget_recall();
        self.prompt = Prompt::Command;
        self.clear_pending();
        self.search.clear();
        // Diagnostics were about the text that was here, and it is not.
        self.diagnostics.clear();
        self.selection = None;
        self.message = format!("\"{}\"", self.buffer.display_name());
        self.redraw = true;
        Ok(())
    }

    /// Put the document that is on screen back into its slot, so another can
    /// take its place. Everything that follows the text through a file is
    /// swapped in one go, and the slot ends up holding the placeholders.
    fn save_document(&mut self) {
        let index = self.current_document();
        let slot = &mut self.documents[index];
        // The cursor on screen is where this document was last looked at.
        slot.cursor = self.cursor;
        std::mem::swap(&mut slot.buffer, &mut self.buffer);
        std::mem::swap(&mut slot.history, &mut self.history);
        std::mem::swap(&mut slot.search, &mut self.search);
        std::mem::swap(&mut slot.highlighter, &mut self.highlighter);
        std::mem::swap(&mut slot.diagnostics, &mut self.diagnostics);
        std::mem::swap(&mut slot.marks, &mut self.marks);
        std::mem::swap(&mut slot.jumps, &mut self.jumps);
        std::mem::swap(&mut slot.jump_from, &mut self.jump_from);
        std::mem::swap(&mut slot.changes, &mut self.changes);
        std::mem::swap(&mut slot.list, &mut self.list);
        std::mem::swap(&mut slot.list_targets, &mut self.list_targets);
        std::mem::swap(&mut slot.last_visual, &mut self.last_visual);
        std::mem::swap(&mut slot.insert_again_at, &mut self.insert_again_at);
    }

    /// The other half of [`Editor::save_document`]: bring the document of the
    /// current window to the screen.
    fn load_document(&mut self) {
        let index = self.current_document();
        let slot = &mut self.documents[index];
        std::mem::swap(&mut slot.buffer, &mut self.buffer);
        std::mem::swap(&mut slot.history, &mut self.history);
        std::mem::swap(&mut slot.search, &mut self.search);
        std::mem::swap(&mut slot.highlighter, &mut self.highlighter);
        std::mem::swap(&mut slot.diagnostics, &mut self.diagnostics);
        std::mem::swap(&mut slot.marks, &mut self.marks);
        std::mem::swap(&mut slot.jumps, &mut self.jumps);
        std::mem::swap(&mut slot.jump_from, &mut self.jump_from);
        std::mem::swap(&mut slot.changes, &mut self.changes);
        std::mem::swap(&mut slot.list, &mut self.list);
        std::mem::swap(&mut slot.list_targets, &mut self.list_targets);
        std::mem::swap(&mut slot.last_visual, &mut self.last_visual);
        std::mem::swap(&mut slot.insert_again_at, &mut self.insert_again_at);
        // Where the document was last looked at. A window that is being
        // switched to puts its own cursor back on top of this one.
        self.cursor = slot.cursor;
    }

    /// Forget the document that is on screen, leaving a placeholder in the live
    /// fields. What was there is dropped, not stored: the document is going
    /// away.
    fn drop_document(&mut self) {
        let mut fresh = Document::placeholder();
        std::mem::swap(&mut fresh.buffer, &mut self.buffer);
        std::mem::swap(&mut fresh.history, &mut self.history);
        std::mem::swap(&mut fresh.search, &mut self.search);
        std::mem::swap(&mut fresh.highlighter, &mut self.highlighter);
        std::mem::swap(&mut fresh.diagnostics, &mut self.diagnostics);
        std::mem::swap(&mut fresh.marks, &mut self.marks);
        std::mem::swap(&mut fresh.jumps, &mut self.jumps);
        std::mem::swap(&mut fresh.jump_from, &mut self.jump_from);
        std::mem::swap(&mut fresh.changes, &mut self.changes);
        std::mem::swap(&mut fresh.list, &mut self.list);
        std::mem::swap(&mut fresh.list_targets, &mut self.list_targets);
        std::mem::swap(&mut fresh.last_visual, &mut self.last_visual);
        std::mem::swap(&mut fresh.insert_again_at, &mut self.insert_again_at);
    }

    /// Where every window is on screen, in cells, for the keys that move
    /// between them and for the renderer that draws them.
    ///
    /// The tree is walked with the whole screen at the root; a split hands its
    /// children equal shares of its rectangle along its axis, keeping one row
    /// or column for the separator between them.
    pub fn window_rects(&self) -> Vec<Rect> {
        let mut rects = vec![Rect::default(); self.windows.len()];
        layout_rects(
            &self.layout,
            Rect {
                x: 0,
                y: 0,
                width: self.viewport.width,
                // The bottom row of the screen is the message row: the windows
                // divide what is above it.
                height: self.viewport.height.saturating_sub(1),
            },
            &mut rects,
        );
        rects
    }

    /// Which document the window that is on screen shows.
    pub fn current_document(&self) -> usize {
        self.windows[self.current_window].document
    }

    /// Put the window that is on screen back into its slot. The size in the
    /// viewport is the screen's and stays where it is: only the scroll and the
    /// cursor belong to the window.
    fn save_window(&mut self) {
        let folds = std::mem::take(&mut self.folds);
        let window = &mut self.windows[self.current_window];
        window.cursor = self.cursor;
        window.top_line = self.viewport.top_line;
        window.left_col = self.viewport.left_col;
        window.folds = folds;
    }

    /// The other half of [`Editor::save_window`].
    fn load_window(&mut self) {
        let window = &self.windows[self.current_window];
        self.cursor = window.cursor;
        self.viewport.top_line = window.top_line;
        self.viewport.left_col = window.left_col;
        self.folds = window.folds.clone();
    }

    /// Show the document at `index` in the window that is up. Its position is
    /// the window's, kept and clamped into the text that arrives.
    pub fn show_document(&mut self, index: usize) -> bool {
        if index >= self.documents.len() || index == self.current_document() {
            return false;
        }
        self.save_document();
        self.windows[self.current_window].document = index;
        self.load_document();
        // The folds were line numbers in the file that was here, and mean nothing in
        // this one. (Vim's manual says the same: "the manual folds are lost when you
        // abandon the file".)
        self.folds.clear();
        self.windows[self.current_window].folds.clear();
        self.clamp_cursor();
        self.after_switch();
        true
    }

    /// Put the tab that is on screen back into its slot.
    fn save_tab(&mut self) {
        let tab = &mut self.tabs[self.current_tab];
        tab.windows = std::mem::take(&mut self.windows);
        tab.layout = self.layout.clone();
        tab.current_window = self.current_window;
    }

    /// The other half of [`Editor::save_tab`].
    fn load_tab(&mut self) {
        let tab = &mut self.tabs[self.current_tab];
        self.windows = std::mem::take(&mut tab.windows);
        self.layout = tab.layout.clone();
        self.current_window = tab.current_window;
    }

    /// Show the tab at `index`. Says whether anything changed.
    pub fn switch_to_tab(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.current_tab {
            return false;
        }
        self.save_window();
        self.save_document();
        self.save_tab();
        self.current_tab = index;
        self.load_tab();
        self.load_document();
        self.load_window();
        self.after_switch();
        true
    }

    /// Open a tab: one window on `document`, or on an empty file when nothing
    /// was named. The new tab becomes the current one, the way vim leaves it.
    pub fn open_tab(&mut self, document: Option<usize>) -> usize {
        self.save_window();
        self.save_document();
        self.save_tab();
        let document = match document {
            Some(document) => document,
            None => {
                self.documents.push(Document::placeholder());
                self.documents.len() - 1
            }
        };
        self.tabs
            .push(Tab::new(vec![Window::new(document)], Layout::Leaf(0)));
        self.current_tab = self.tabs.len() - 1;
        self.load_tab();
        self.load_document();
        self.load_window();
        self.after_switch();
        self.current_tab
    }

    /// Close the tab that is on screen. The last one is not closed: a session
    /// with no tab has nowhere to show anything.
    pub fn close_tab(&mut self) -> bool {
        if self.tabs.len() < 2 {
            return false;
        }
        self.save_window();
        self.save_document();
        self.save_tab();
        self.tabs.remove(self.current_tab);
        if self.current_tab >= self.tabs.len() {
            self.current_tab = self.tabs.len() - 1;
        }
        self.load_tab();
        self.load_document();
        self.load_window();
        self.after_switch();
        true
    }

    /// Show the next or previous tab, wrapping around.
    pub fn step_tab(&mut self, step: isize) -> bool {
        let count = self.tabs.len() as isize;
        if count < 2 {
            return false;
        }
        let next = (self.current_tab as isize + step).rem_euclid(count) as usize;
        self.switch_to_tab(next)
    }

    /// Show the window at `index`. Says whether anything changed.
    pub fn switch_to_window(&mut self, index: usize) -> bool {
        if index >= self.windows.len() || index == self.current_window {
            return false;
        }
        self.save_window();
        self.save_document();
        self.current_window = index;
        self.load_document();
        self.load_window();
        self.after_switch();
        true
    }

    /// What has to be let go of when the text underneath it changes: anything
    /// holding character indices into the document that was on screen.
    fn after_switch(&mut self) {
        self.mode = Mode::Normal;
        self.selection = None;
        self.mouse_dragged = false;
        self.clear_pending();
        self.cmdline.clear();
        self.cmdline_cursor = 0;
        self.forget_recall();
        self.prompt = Prompt::Command;
        self.message = format!("\"{}\"", self.buffer.display_name());
        self.redraw = true;
    }

    /// Open `path` as a document of its own and show it in the window that is
    /// up. The file is read first, so a path that is not there leaves the editor
    /// as it was.
    pub fn open_file(&mut self, path: &Path) -> Result<(), BufferError> {
        let buffer = Buffer::from_path(path)?;
        let language = Language::from_path(path);
        self.save_document();
        self.documents.push(Document::new(buffer, language));
        self.windows[self.current_window].document = self.documents.len() - 1;
        self.load_document();
        // The folds belonged to the file that was here: they are line numbers in
        // *its* text, and mean nothing in this one. (Vim's manual says the same —
        // "the manual folds are lost when you abandon the file".)
        self.folds.clear();
        self.windows[self.current_window].folds.clear();
        // A file that arrives is looked at from the top: the position the
        // window had belongs to whatever it was showing before.
        self.cursor = Cursor::default();
        self.viewport.top_line = 0;
        self.viewport.left_col = 0;
        self.after_switch();
        Ok(())
    }

    /// Open `path` as a document of its own, without showing it anywhere.
    ///
    /// This is what `:sp file` needs: the file has to join the list, and the
    /// window that asked for the split keeps what it had.
    pub fn add_file(&mut self, path: &Path) -> Result<usize, BufferError> {
        let buffer = Buffer::from_path(path)?;
        let language = Language::from_path(path);
        self.documents.push(Document::new(buffer, language));
        Ok(self.documents.len() - 1)
    }

    /// Split the window that is up along `axis`, showing `document` (or the
    /// same document as the window it came from) in the new one. The new window
    /// becomes the current one, the way vim leaves it.
    pub fn split_window(&mut self, axis: Axis, document: Option<usize>) -> usize {
        self.save_window();
        self.save_document();
        let beside = self.current_window;
        let document = document.unwrap_or_else(|| self.windows[beside].document);
        let window = Window {
            document,
            cursor: self.cursor,
            top_line: self.viewport.top_line,
            left_col: self.viewport.left_col,
            // A window that is made from this one starts with its folds: they are
            // about the file, and a person who split to see two places in it did
            // not ask for the folds to be forgotten.
            folds: self.folds.clone(),
        };
        self.windows.push(window);
        let window = self.windows.len() - 1;
        self.layout.split(beside, axis, window);
        self.current_window = window;
        // The new window shows a document that may or may not be the one that
        // was live: bring its state out.
        self.load_document();
        self.clamp_cursor();
        self.after_switch();
        window
    }

    /// Close the window at `index`. Closing the last window leaves an empty one
    /// behind, the way a window that has lost its file keeps its place on
    /// screen — the session quits by other means.
    pub fn close_window(&mut self, index: usize) {
        // A window showing a *list* has no file behind it: the document was made
        // for the window, and closing the window is what takes it away. Without
        // this every list a session opened would stay in `documents` for the rest
        // of the session, which is a leak a person can see in `:ls`.
        //
        // The document on screen keeps its state in the live fields and only a
        // placeholder in its slot — the line this project keeps meeting — so the
        // question "is this a list?" is asked of the live field when it is the
        // live document.
        let showing = self.windows.get(index).map(|window| window.document);
        if let Some(document) = showing {
            let is_list = if document == self.current_document() {
                self.list.is_some()
            } else {
                self.documents
                    .get(document)
                    .is_some_and(|d| d.list.is_some())
            };
            if is_list {
                self.close_document(document);
                return;
            }
        }
        let was_current = index == self.current_window;
        if was_current {
            // The live window and its document go back to their slots: the
            // document is still open even when nothing shows it.
            self.save_document();
            self.drop_window();
        }
        self.windows.remove(index);
        self.layout.remove(index);
        renumber(&mut self.layout, index);
        if self.windows.is_empty() {
            // Nothing is left to show a document: a window on whatever there
            // is, or an empty document if there is nothing at all.
            if self.documents.is_empty() {
                self.documents.push(Document::placeholder());
            }
            self.windows.push(Window::new(0));
            self.layout = Layout::Leaf(0);
            self.current_window = 0;
            self.load_document();
            self.load_window();
        } else {
            if index < self.current_window {
                self.current_window -= 1;
            }
            self.current_window = self.current_window.min(self.windows.len() - 1);
            // The window that is up now may show a different document.
            self.load_document();
            self.load_window();
            self.clamp_cursor();
        }
        self.after_switch();
    }

    /// Close a document, and every window that was showing it.
    pub fn close_document(&mut self, index: usize) {
        if self.current_document() == index {
            // The document is going: nothing keeps its state, and the live
            // window is one of the windows that goes with it.
            self.drop_document();
            self.drop_window();
        }
        let doomed: Vec<usize> = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, window)| window.document == index)
            .map(|(at, _)| at)
            .collect();
        // From the highest down, so the positions of the ones still to go hold.
        for at in doomed.iter().rev() {
            self.windows.remove(*at);
            self.layout.remove(*at);
            renumber(&mut self.layout, *at);
        }
        self.documents.remove(index);
        for window in &mut self.windows {
            if window.document > index {
                window.document -= 1;
            }
        }
        if self.windows.is_empty() {
            if self.documents.is_empty() {
                self.documents.push(Document::placeholder());
            }
            self.windows.push(Window::new(0));
            self.layout = Layout::Leaf(0);
            self.current_window = 0;
        } else {
            self.current_window = self.current_window.min(self.windows.len() - 1);
        }
        self.load_document();
        self.load_window();
        self.clamp_cursor();
        self.after_switch();
    }

    /// Keep the cursor on a position that exists in the document that is up.
    /// A window looking at another file keeps the position it had, clamped.
    fn clamp_cursor(&mut self) {
        let last = self.buffer.line_count().saturating_sub(1);
        self.cursor.row = self.cursor.row.min(last);
        let len = self.buffer.line_len(self.cursor.row);
        self.cursor.col = self.cursor.col.min(len);
        if self.mode != Mode::Insert {
            self.clamp_normal_cursor();
        }
    }

    /// Make the window that is up bigger or smaller along `axis`.
    ///
    /// `Some(n)` sets its size to `n` rows or columns, `Some(-n)` takes `n`
    /// away, and `None` adds one — vim's `:resize`, `:resize -2` and `:resize`
    /// with nothing after it. The nearest division that runs along the axis is
    /// the one that moves; a window with no such division is the whole of its
    /// rectangle and does not move at all.
    pub fn resize_window(&mut self, axis: Axis, size: Resize) -> bool {
        let window = self.current_window;
        self.resize_window_at(window, axis, size)
    }

    /// The same, for a window named rather than the one that is up: a dragged
    /// separator moves the window on one side of it.
    pub fn resize_window_at(&mut self, window: usize, axis: Axis, size: Resize) -> bool {
        let Some(path) = layout_path(&self.layout, window) else {
            return false;
        };
        // What the window is now, for the first `:resize`: a window that has
        // never been given a size takes the size it is drawn at, so the first
        // one starts from what is on screen rather than from nothing.
        let now = self
            .window_rects()
            .get(window)
            .map(|rect| match axis {
                Axis::Rows => rect.height.saturating_sub(1),
                Axis::Columns => rect.width,
            })
            .unwrap_or(1)
            .max(1);
        let mut node = &mut self.layout;
        for step in path {
            let Layout::Split {
                axis: here,
                sizes,
                children,
            } = node
            else {
                return false;
            };
            if *here == axis {
                let slot = &mut sizes[step];
                *slot = match size {
                    Resize::One => slot.saturating_add(1),
                    Resize::Set(n) => n,
                    Resize::By(n) if n >= 0 => slot.saturating_add(n.min(u16::MAX as i32) as u16),
                    Resize::By(n) => {
                        slot.saturating_sub(n.unsigned_abs().min(u16::MAX as u32) as u16)
                    }
                };
                if *slot == 0 {
                    *slot = now;
                }
                self.redraw = true;
                return true;
            }
            node = &mut children[step];
        }
        false
    }

    /// Give every window in this tab its equal share again.
    pub fn equalize_windows(&mut self) {
        clear_sizes(&mut self.layout);
        self.redraw = true;
    }

    /// Close every window but this one.
    pub fn close_other_windows(&mut self) {
        self.save_window();
        self.save_document();
        let kept = self.windows[self.current_window].clone();
        self.windows = vec![kept];
        self.layout = Layout::Leaf(0);
        self.current_window = 0;
        self.load_document();
        self.load_window();
        self.after_switch();
    }

    /// Forget the window that is on screen, leaving a placeholder in the live
    /// fields. What was there is dropped, not stored.
    fn drop_window(&mut self) {
        self.cursor = Cursor::default();
        self.viewport.top_line = 0;
        self.viewport.left_col = 0;
    }

    // -- rendering ----------------------------------------------------------

    /// The status line text.
    pub fn status(&self) -> String {
        let status = status_line(
            &self.mode_label(),
            &self.buffer.display_name(),
            self.is_dirty(),
            self.cursor,
            &self.diagnostics,
        );
        // Vim's tabline, in the one row there is for it: which tab of how many,
        // and only when there is more than one to be in.
        if self.tabs.len() > 1 {
            return format!("{status}  [{}/{}]", self.current_tab + 1, self.tabs.len());
        }
        status
    }

    /// Whether the document on screen has been changed since it was last
    /// written.
    ///
    /// Nothing is stored for this. The undo history hands out an identity to
    /// every state its steps produce, and the last write remembers which state
    /// that was: `u` back to it clears the marker and redoing away from it puts
    /// the marker back, with nothing comparing text and nothing to get out of
    /// date. An edit that happens to restore the old text is still an edit —
    /// which is what vim does, and what keeps `:q` honest.
    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty()
    }

    /// How many edits this document has had. It only ever grows, which is what
    /// makes it a version number: a client that has sent the text as of edit N
    /// has nothing to send until the count moves.
    pub fn edit_count(&self) -> u64 {
        self.history.edits()
    }

    /// Whether the line at the bottom is being typed into.
    fn in_prompt(&self) -> bool {
        self.mode == Mode::Command
    }

    /// What the status line calls the current state: the mode, and what a visual
    /// selection covers, which is the difference between taking characters and
    /// taking whole lines.
    fn mode_label(&self) -> String {
        match (self.mode, self.selection) {
            (Mode::Visual, Some(selection)) => {
                format!("{}{}", self.mode.name(), selection.kind.name())
            }
            _ => self.mode.name().to_string(),
        }
    }

    /// Compose the whole screen.
    pub fn compose(&self) -> Composed {
        compose(&self.view())
    }

    // -- undoable edits ----------------------------------------------------

    /// Replace the characters in `[start, end)` with `text`, as one undoable
    /// change.
    ///
    /// The cursor ends at character index `after`, or stays where it is when
    /// `after` is `None`; either way [`Editor::after_edit`] clamps it
    /// afterwards. Every edit a command makes goes through here, which is what
    /// keeps the history and the buffer in step.
    pub fn replace_range(&mut self, start: usize, end: usize, text: &str, after: Option<usize>) {
        if self.list.is_some() {
            // A list that can be edited is a list that can lie about what it
            // says.
            self.message = "E21: Cannot make changes, 'readonly' option is set".to_string();
            return;
        }
        let change = Change::planned(&self.buffer, start, end, text, self.cursor);
        // A fold holds *line numbers*, and the edit is about to move the lines
        // around it: what is done here is what keeps a fold on the text it was put
        // on. Where the edit starts is read before the change, since after it the
        // index means something else; how many lines it made or unmade is read from
        // the buffer, which is the only thing that knows — typing a character into
        // the middle of a line adds no line at all.
        let fold_at = self.fold_line_for(start);
        self.record_fold_state();
        let lines_before = self.buffer.line_count();
        self.buffer.replace_range(start, end, text);
        if let Some(at) = fold_at {
            let delta = self.buffer.line_count() as isize - lines_before as isize;
            self.shift_folds(at, delta);
        }
        // And the state this edit *leaves*, one index on: the history is about to gain
        // this step, and a redo back to it has to find the folds it produced.
        self.fold_states.truncate(self.history.steps() + 1);
        self.fold_states.push(self.folds.clone());
        if let Some(index) = after {
            self.cursor.set_char_idx(&self.buffer, index);
        }
        // Every edit in the kernel comes through here, which is what makes this
        // the one place the change list has to be told about.
        self.changes.record(self.cursor);
        // And the one place a snippet's mirrors hear about an edit: what is
        // typed into a stop is what the other writings of that number say.
        self.sync_snippet_mirrors(start, end, text.chars().count());
        self.after_edit();
        // The cursor the caller asked for says where the next text goes: `o`
        // asks for the start of the line break it inserted, and the typing that
        // follows belongs in front of it.
        let caret = after.map_or(0, |index| index.saturating_sub(start));
        self.history
            .push(change.settled(self.cursor).with_caret(caret));
        // A match is a pair of character indices, and the edit has just moved
        // them: the search looks again rather than pointing at the wrong text.
        self.search.refresh(&self.buffer);
        // A line's syntax state depends on the lines above it, so an edit
        // forgets the states from its own line onwards — and nothing above it,
        // which is what keeps this cheap.
        self.highlighter
            .invalidate(self.buffer.char_to_line_col(start).0);
    }

    /// Take `[start, end)` out of the buffer and into the register, as one
    /// undoable change.
    ///
    /// This is what every delete an operator makes goes through, so what `p`
    /// will put is always what the last delete took.
    pub fn cut(&mut self, start: usize, end: usize, kind: Kind, after: Option<usize>) {
        let text = self.buffer.slice_text(start, end);
        self.store(Register::new(text, kind), false);
        self.replace_range(start, end, "", after);
    }

    /// Copy `[start, end)` into the register, and leave the buffer alone.
    pub fn copy(&mut self, start: usize, end: usize, kind: Kind) {
        let text = self.buffer.slice_text(start, end);
        self.store(Register::new(text, kind), true);
    }

    /// Put `register` where the keystroke that asked for it says.
    ///
    /// The unnamed register takes everything — it is what `p` uses without being
    /// asked — unless the black hole was named, which exists precisely to take
    /// something without leaving a trace. A yank also fills `"0`; a delete
    /// shifts the numbered registers along. The clipboard register is both kept
    /// here and handed to the frontend, which is the half of it that reaches the
    /// system clipboard.
    fn store(&mut self, register: Register, yanked: bool) {
        let name = match self.register_prefix {
            Prefix::Named(name) => Some(name),
            Prefix::None | Prefix::Awaiting => None,
        };
        // The black hole takes text without leaving a trace anywhere: not the
        // unnamed register, not the numbered history, not `"0`.
        let swallowed = name == Some('_');
        if !swallowed {
            self.register = register.clone();
            if yanked {
                self.registers.note_yank(&register);
            } else {
                self.registers.note_delete(&register);
            }
        }
        match name {
            None | Some('_') => {}
            Some('+') | Some('*') => {
                self.registers.set('+', register.clone());
                self.clipboard = Some(register.text().to_string());
            }
            Some(name) => self.registers.set(name, register),
        }
        // The register was asked for by a key that has now been used.
        self.register_prefix = Prefix::None;
    }

    /// Type what an insert session typed, and leave insert mode.
    ///
    /// The keys that only moved the cursor around inside the session are not in
    /// the log: what `.` repeats is the text, typed where the cursor is now. A
    /// session that was a paste, or one whose text came from an input method,
    /// repeats the same way a typed one does.
    pub fn replay_insert(&mut self, log: &str) {
        if self.mode != Mode::Insert {
            self.mode = Mode::Insert;
        }
        for c in log.chars() {
            match c {
                '\u{8}' => self.delete_backward(),
                '\u{7f}' => {
                    self.run_command("delete_forwards", &[]);
                }
                _ => self.type_text(&c.to_string()),
            }
        }
        self.run_command("escape", &[]);
    }

    /// The separator at a screen cell, if there is one, and the window on the
    /// near side of it.
    ///
    /// The answer comes from the rectangles rather than from the tree, because
    /// the rectangles are what was drawn: a vertical separator is the column
    /// just left of a window that is not the first, and a horizontal one is the
    /// last row of a window that has another below it — which is also its status
    /// line, since a window keeps its own.
    pub fn separator_at(&self, x: u16, y: u16) -> Option<Resizing> {
        let rects = self.window_rects();
        for (index, rect) in rects.iter().enumerate() {
            if rect.width == 0 || rect.height == 0 {
                continue;
            }
            // The column between two side-by-side windows.
            if rect.x > 0 && x == rect.x - 1 && y >= rect.y && y < rect.y + rect.height {
                // The window on the near side ends *at* the separator column.
                let left = rects
                    .iter()
                    .position(|other| other.x + other.width == rect.x - 1)?;
                let width = rects[left].width;
                return Some(Resizing {
                    window: left,
                    axis: Axis::Columns,
                    base: width,
                    at: x,
                });
            }
            // The row between two stacked windows: the upper one's status row.
            let bottom = rect.y + rect.height;
            if x >= rect.x
                && x < rect.x + rect.width
                && y + 1 == bottom
                && rects.iter().any(|other| other.y == bottom)
            {
                return Some(Resizing {
                    window: index,
                    axis: Axis::Rows,
                    base: rect.height.saturating_sub(1),
                    at: y,
                });
            }
        }
        None
    }

    /// What `p` will put: the register a `"x` named, or the unnamed one.
    pub fn chosen_register(&self) -> &Register {
        match self.register_prefix {
            Prefix::Named(name) => self.registers.get(name).unwrap_or(&self.register),
            Prefix::None | Prefix::Awaiting => &self.register,
        }
    }

    /// Take a bracketed paste as what the system clipboard holds, so `"+p` can
    /// put back what the terminal just pasted.
    ///
    /// The kernel cannot read a clipboard, so this is the only way it learns:
    /// a paste is the clipboard arriving, and it is remembered at that moment.
    /// An input method's commit comes in the same message and is remembered the
    /// same way, which is a small lie about a register nobody asked.
    pub fn note_clipboard_text(&mut self, text: &str) {
        self.registers.set_clipboard(text.to_string());
    }

    /// Undo the most recent change: put the text and the cursor back.
    ///
    /// Returns `false` when there is nothing left to undo.
    pub fn undo(&mut self) -> bool {
        match self.history.undo(&mut self.buffer) {
            Some(cursor) => {
                self.cursor = cursor;
                self.restore_folds();
                self.after_edit();
                // Undo moves the cursor to the change it put back, so the
                // states to forget are the ones from that line onwards.
                self.highlighter.invalidate(self.cursor.row);
                true
            }
            None => false,
        }
    }

    /// Redo the change that was undone last.
    ///
    /// Returns `false` when there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        match self.history.redo(&mut self.buffer) {
            Some(cursor) => {
                self.cursor = cursor;
                self.restore_folds();
                self.after_edit();
                self.highlighter.invalidate(self.cursor.row);
                true
            }
            None => false,
        }
    }

    // -- helpers used by commands ------------------------------------------

    /// Call after anything that may have moved the cursor or changed the
    /// buffer: it re-clamps the cursor, scrolls it back into view and marks the
    /// screen dirty.
    pub fn after_edit(&mut self) {
        let came_from = self.last_cursor_row;
        // The row is always clamped; the column is not while a block is being made,
        // since a rectangle may reach past the end of a short line and `Cursor::clamp`
        // would pull it back to the last character of one.
        self.cursor.row = self
            .cursor
            .row
            .min(self.buffer.line_count().saturating_sub(1));
        if !self.is_making_a_block() {
            self.cursor.col = self.cursor.col.min(self.buffer.line_len(self.cursor.row));
        }
        self.clamp_normal_cursor();
        // A closed fold hides the lines after its first, and a cursor inside one has
        // nowhere to be: every edit settles it, which is what keeps `j` from walking
        // into a fold it cannot see.
        self.settle_cursor_from(came_from);
        self.last_cursor_row = self.cursor.row;
        self.reposition();
        self.redraw = true;
    }

    fn reposition(&mut self) {
        let options = self.options.borrow().clone();
        let gutter = gutter_width(self.buffer.line_count(), options.number);
        scroll_to_cursor(
            &mut self.viewport,
            self.cursor,
            &self.buffer,
            gutter,
            options.scrolloff.0,
        );
        // A fold hides the lines after its first one, and the view cannot start on
        // a line that is not drawn: the top snaps to the nearest line that is. (The
        // scroll is worked out in lines and not in drawn rows, so `'scrolloff'` can
        // be a line or two out when folds are closed — the cursor is always on
        // screen, which is what the setting is for.)
        if self.line_is_folded(self.viewport.top_line) {
            let row = (0..self.viewport.top_line)
                .rev()
                .find(|row| !self.line_is_folded(*row));
            self.viewport.top_line = row.unwrap_or(0);
        }
    }

    /// Scroll the view a line at a time, taking the cursor with it when the
    /// screen would otherwise leave it behind.
    ///
    /// This is the one place where the screen moves on its own: `<C-e>` and
    /// `<C-y>` read *ahead* of where the cursor is, which is what a person does
    /// when they are looking rather than editing. The cursor goes with the text
    /// by the same amount — vim's rule, and the reason it stays on the same
    /// screen row — and stops `scrolloff` lines short of the edge, so the context
    /// the setting promises is kept even here. At the ends of the file the view
    /// does not move, and so neither does the cursor.
    pub fn scroll_view_by(&mut self, lines: i64) {
        let before = self.viewport.top_line;
        self.scroll_view(lines);
        let moved = self.viewport.top_line as i64 - before as i64;
        let rows = self.viewport.text_rows() as usize;
        if rows == 0 || moved == 0 {
            return;
        }
        let last = self.buffer.line_count().saturating_sub(1);
        self.cursor.row = (self.cursor.row as i64 + moved).clamp(0, last as i64) as usize;
        let margin = self
            .options
            .borrow()
            .scrolloff
            .0
            .min(rows.saturating_sub(1) / 2);
        let top = self.viewport.top_line;
        let bottom = top + rows - 1;
        if self.cursor.row < top + margin {
            self.cursor.row = (top + margin).min(bottom);
        } else if self.cursor.row + margin > bottom {
            self.cursor.row = bottom.saturating_sub(margin).max(top);
        }
    }

    /// Put the cursor on the first character of its line that is not blank.
    ///
    /// `^`, `{letter}'` and the screen motions `H`/`M`/`L` all mean this rather
    /// than column zero: a line that is indented is *about* the text, and a
    /// cursor left in the indentation is not where a person meant to be.
    pub fn move_to_first_non_blank(&mut self) {
        let row = self.cursor.row;
        let len = self.buffer.line_len(row);
        let line_start = self.buffer.line_col_to_char(row, 0);
        let col = (0..len)
            .find(|index| {
                self.buffer
                    .char_at(line_start + index)
                    .is_some_and(|c| !c.is_whitespace())
            })
            .unwrap_or(0);
        self.cursor.col = col;
    }

    /// Keep the cursor off the line terminator.
    ///
    /// Insert mode belongs *between* characters, so `col` may sit one past the
    /// last one. Normal mode does not: vim puts the cursor on the last
    /// character, which is also what makes `x`, `$` and `l` behave.
    pub(crate) fn clamp_normal_cursor(&mut self) {
        if !matches!(self.mode, Mode::Normal | Mode::Visual) {
            return;
        }
        // The row first: a command that takes lines away — `J` joins several into
        // one — leaves the cursor's row past the end until something clamps it, and
        // asking for the length of a line that is not there is a panic rather than
        // an answer.
        let last = self.buffer.line_count().saturating_sub(1);
        self.cursor.row = self.cursor.row.min(last);
        // While a *block* is being made the column is free: the cursor may sit past
        // the last character of a short line, which is what a rectangle reaching into
        // one needs. Normal mode never lets it past the last character.
        if self.is_making_a_block() {
            return;
        }
        let line_len = self.buffer.line_len(self.cursor.row);
        self.cursor.col = if line_len == 0 {
            0
        } else {
            self.cursor.col.min(line_len - 1)
        };
    }

    /// Run a named command, then drain anything Lua queued while running it.
    pub fn run_command(&mut self, name: &str, params: &[Param]) {
        let name = name.to_string();
        // One command is one step in the history — a counted `3x` is a single
        // change to a user — and so is a whole insert session. The two nest:
        // the group opened here closes before this returns, and entering or
        // leaving insert mode opens or closes the session's group around it.
        let was_insert = self.mode == Mode::Insert;
        let edits_before = self.history.edits();
        self.history.begin_group();
        let mut next = Some((name.clone(), params.to_vec()));
        let mut depth = 0;
        while let Some((name, params)) = next.take() {
            if let Err(message) = self.run_named(&name, &params) {
                self.message = message;
                break;
            }
            depth += 1;
            if depth > MAX_COMMAND_DEPTH {
                self.message = "E5108: command nesting too deep".to_string();
                break;
            }
            next = self.lua.take_pending();
        }
        let entered_insert = self.mode == Mode::Insert && !was_insert;
        let left_insert = self.mode != Mode::Insert && was_insert;
        // A `:s///c` keeps the group its command opened: the answers are keys,
        // and every replacement they make has to join that one step.
        let asking = self.confirm.is_some();
        // What `.` will repeat: a command that changed something, or one that
        // opened an insert session — `ihello<Esc>` changes nothing until the
        // typing arrives, and it is the typing that is the change.
        let recorded = self.history.edits() != edits_before || entered_insert;
        let repeatable = !matches!(name.as_str(), "undo" | "redo" | "repeat" | "escape");
        if recorded && repeatable {
            self.last_command = Some((name.clone(), params.to_vec()));
            if !entered_insert {
                // A change with no session behind it: the text of the last one
                // is not part of this one.
                self.last_insert = None;
            }
        }
        if entered_insert {
            self.insert_log = Some(String::new());
        }
        if left_insert {
            let typed = self.insert_log.take().unwrap_or_default();
            self.last_insert = (!typed.is_empty()).then_some(typed);
        }
        if !entered_insert && !asking {
            self.history.end_group(&mut self.buffer);
        }
        if left_insert {
            // The session's group closes here, so everything typed in it is one
            // step. A command that *entered* insert mode does not close its
            // group above: that group is the session's.
            self.history.end_group(&mut self.buffer);
        }
        self.redraw = true;
    }

    fn run_named(&mut self, name: &str, params: &[Param]) -> Result<(), String> {
        // Copy the function pointer out so the registry borrow ends before the
        // editor is borrowed mutably.
        if let Some(f) = self.registry.get(name) {
            return f(self, params).map_err(|e| e.to_string());
        }
        if self.lua.has_command(name) {
            return self
                .lua
                .call_command(name, params)
                .map_err(|e| e.to_string());
        }
        Err(CommandError::Unknown(name.to_string()).to_string())
    }

    // -- mode handlers ------------------------------------------------------

    fn handle_normal(&mut self, key: KeyMsg) {
        self.clamp_normal_cursor();
        let encoded = encode_key(&key);

        // A `:s///c` is asking a question and every key is an answer to it.
        if self.confirm.is_some() {
            self.answer_confirm(&encoded);
            return;
        }
        // While a macro is recording, `q` is what stops it — vim's rule, and the
        // one key that means two things at once: `q:` and `q/` are the history
        // windows, and a recording ends before either can be reached. (Vim would
        // record the pair and open the window; ending the recording is the reading
        // a person expects, and the roadmap says so.)
        if self.recording.is_some() && encoded == "q" {
            let (recording, keys) = self.recording.take().expect("just looked at it");
            let keys = keys.strip_suffix('q').unwrap_or(&keys).to_string();
            let index = (recording as u8).wrapping_sub(b'a') as usize;
            self.macros[index] = Some(keys);
            self.message = format!("recorded @{recording}");
            return;
        }
        // `q` and `@` are waiting for a register letter, and until it comes no
        // other key means anything. (`:` and `/` after a `q` are not a register
        // but the history windows: the keymap is given those.)
        if self.macro_ask.is_some() && self.take_macro_letter(&encoded) {
            return;
        }
        // `m`, `` ` `` and `'` are waiting for a letter, and until it comes no
        // other key means anything.
        if self.mark_prefix.is_some() {
            self.take_mark_target(&encoded);
            return;
        }

        // A `"` names the register for the next operator, yank or put: `"ayy`,
        // `"ap`, `\"_dd`. It comes before the operator, never after.
        match self.register_prefix {
            Prefix::Awaiting => {
                match encoded.chars().next() {
                    Some(name) if is_register_name(name) => {
                        self.register_prefix = Prefix::Named(name);
                    }
                    _ => {
                        self.message = format!("E354: Invalid register name: {encoded}");
                        self.register_prefix = Prefix::None;
                    }
                }
                return;
            }
            Prefix::None if encoded == "\"" && self.operator.is_none() => {
                self.register_prefix = Prefix::Awaiting;
                return;
            }
            _ => {}
        }

        // `f{char}` and its relatives wait for their character, which the
        // keymap cannot carry.
        if let Some(name) = pending_find(&self.pending) {
            self.take_find_target(&encoded, name);
            return;
        }
        if self.operator.is_none()
            && self.register_prefix == Prefix::None
            && (self.starts_find(&encoded) || self.starts_mark(&encoded))
        {
            return;
        }

        // `@` plays a macro and is not in the keymap — the letter that follows is
        // any letter, which a table of names cannot say.
        if self.pending.is_empty() && self.operator.is_none() && encoded == "@" {
            self.macro_ask = Some(MacroAsk::Play);
            return;
        }

        if self.operator.is_some() {
            self.handle_operator_pending(key, &encoded);
        } else if let Some(digit) = count_digit(&key, !self.pending_count.is_empty()) {
            if self.pending_count.len() < MAX_COUNT_DIGITS {
                self.pending_count.push(digit);
            }
        } else {
            self.dispatch_normal(&encoded);
        }
        // Motions and edits may have left the cursor on the terminator.
        self.clamp_normal_cursor();
    }

    /// The keymap's turn: resolve the pending sequence, and run what it binds.
    fn dispatch_normal(&mut self, encoded: &str) {
        let sequence = if self.pending.is_empty() {
            encoded.to_string()
        } else {
            format!("{} {}", self.pending, encoded)
        };
        match self.lookup_normal(&sequence) {
            Lookup::Exact(binding) => {
                self.pending.clear();
                let count = self.take_count();
                self.run_binding(binding, count);
            }
            Lookup::Prefix => self.pending = sequence,
            Lookup::None => {
                if self.pending.is_empty() {
                    self.unmapped(encoded);
                    return;
                }
                // `q` and a letter the table does not name is a macro: the
                // recording starts (or stops) here rather than in a second pending
                // mechanism beside the keymap's.
                if self.pending == "q" {
                    let key = encoded.to_string();
                    self.pending.clear();
                    self.macro_ask = Some(MacroAsk::Record);
                    if self.take_macro_letter(&key) {
                        return;
                    }
                }
                // The pending key did not start a binding; retry this key alone.
                self.pending.clear();
                match self.lookup_normal(encoded) {
                    Lookup::Exact(binding) => {
                        let count = self.take_count();
                        self.run_binding(binding, count);
                    }
                    Lookup::Prefix => self.pending = encoded.to_string(),
                    Lookup::None => self.unmapped(encoded),
                }
            }
        }
    }

    /// Visual mode's turn: the keys that act on the selection, and everything
    /// else which extends it.
    ///
    /// Motions are the normal mode's, so the selection is extended by the very
    /// same commands — including a user's remapping of them.
    fn handle_visual(&mut self, key: KeyMsg) {
        if let Some(digit) = count_digit(&key, !self.pending_count.is_empty()) {
            if self.pending_count.len() < MAX_COUNT_DIGITS {
                self.pending_count.push(digit);
            }
            return;
        }
        let encoded = encode_key(&key);

        // `l` and `h` in a *block* move the column and nothing else: a rectangle may
        // reach past the end of a short line — that is the whole point of one, and
        // what vim's `'virtualedit'` allows there — so the motion must not stop at
        // the line's end the way `move_right` does everywhere else.
        if self.is_making_a_block() && matches!(encoded.as_str(), "l" | "h") {
            let count = self.take_count();
            for _ in 0..count {
                if encoded == "l" {
                    self.cursor.col += 1;
                } else {
                    self.cursor.col = self.cursor.col.saturating_sub(1);
                }
            }
            self.after_edit();
            return;
        }

        // `"` names the register for a yank out of the selection: `"+y`.
        match self.register_prefix {
            Prefix::Awaiting => {
                match encoded.chars().next() {
                    Some(name) if is_register_name(name) => {
                        self.register_prefix = Prefix::Named(name);
                    }
                    _ => {
                        self.message = format!("E354: Invalid register name: {encoded}");
                        self.register_prefix = Prefix::None;
                    }
                }
                return;
            }
            Prefix::None if encoded == "\"" => {
                self.register_prefix = Prefix::Awaiting;
                return;
            }
            _ => {}
        }

        // A find moves the selection's end: `vf,`. The character it waits for
        // comes in the same way it does in normal mode.
        if let Some(name) = pending_find(&self.pending) {
            self.take_find_target(&encoded, name);
            return;
        }
        if self.pending.is_empty() && self.starts_find(&encoded) {
            return;
        }

        // `i` and `a` in a selection ask for a text object as well: `viw`
        // selects the word, `va"` the string and its quotes.
        if self.pending.is_empty() {
            if self.object.is_none() && (encoded == "i" || encoded == "a") {
                self.object = Some(if encoded == "i" {
                    Object::Inner
                } else {
                    Object::Around
                });
                return;
            }
            if let Some(object) = self.object {
                self.object = None;
                match command::object_span(self, object, &encoded) {
                    Some(span) if span.start < span.end => {
                        self.select_object(span);
                    }
                    Some(_) => {}
                    None => {
                        self.message = format!("{encoded} is not a text object");
                    }
                }
                return;
            }
        }

        let sequence = if self.pending.is_empty() {
            encoded.clone()
        } else {
            format!("{} {}", self.pending, encoded)
        };

        // A visual binding wins, and the normal mode's motions are what is left.
        match self.lookup_visual(&sequence) {
            Lookup::Exact(binding) => {
                self.pending.clear();
                let count = self.take_count();
                self.run_binding(binding, count);
            }
            Lookup::Prefix => self.pending = sequence,
            Lookup::None => {
                // A multi-key motion is the normal mode's too (`gg`, `dd` is not
                // a motion, but `gg` is), so the fallback looks the sequence up
                // exactly as the normal mode would.
                match self.lookup_normal(&sequence) {
                    Lookup::Exact(binding) => {
                        self.pending.clear();
                        let count = self.take_count();
                        self.run_binding(binding, count);
                    }
                    Lookup::Prefix => self.pending = sequence,
                    Lookup::None => {
                        self.pending.clear();
                        self.unmapped(&encoded);
                    }
                }
            }
        }
        self.clamp_normal_cursor();
    }

    fn lookup_visual(&self, sequence: &str) -> Lookup {
        let keymap = self.keymap.borrow();
        keymap.lookup(Mode::Visual, sequence)
    }

    /// The character range the visual selection covers, `[start, end)`.
    ///
    /// A characterwise selection includes the character the cursor is on; a
    /// linewise one covers whole lines, terminators and all, which is what makes
    /// `V` + `d` the same thing as `dd`.
    pub fn visual_span(&self) -> Option<(usize, usize)> {
        let selection = self.selection?;
        let anchor = selection.anchor.char_idx(&self.buffer);
        let cursor = self.cursor.char_idx(&self.buffer);
        let (start, end) = if selection.kind == VisualKind::Linewise {
            let (first, last) = if selection.anchor.row <= self.cursor.row {
                (selection.anchor.row, self.cursor.row)
            } else {
                (self.cursor.row, selection.anchor.row)
            };
            return Some(command::line_span(self, first, last - first + 1));
        } else if anchor <= cursor {
            (anchor, cursor + 1)
        } else {
            (cursor, anchor + 1)
        };
        (start < end).then_some((start, end))
    }

    /// The rows and columns a blockwise selection covers, as a rectangle.
    ///
    /// Rows are buffer rows and columns are *character* columns within each row,
    /// which is what the cursor counts here: a tab is one column, as it is
    /// everywhere else in this editor. `None` unless `<C-v>` is what selected.
    pub fn visual_block(&self) -> Option<(usize, usize, usize, Option<usize>)> {
        let selection = self.selection?;
        if selection.kind != VisualKind::Blockwise {
            return None;
        }
        let (first, last) = if selection.anchor.row <= self.cursor.row {
            (selection.anchor.row, self.cursor.row)
        } else {
            (self.cursor.row, selection.anchor.row)
        };
        let (left, right) = if selection.anchor.col <= self.cursor.col {
            (selection.anchor.col, self.cursor.col)
        } else {
            (self.cursor.col, selection.anchor.col)
        };
        // `<C-v>$` makes the right edge the end of each line rather than a column,
        // which is the difference between a rectangle and a ragged block.
        Some((first, last, left, (!selection.to_end).then_some(right)))
    }

    /// The character range the block covers on one row, as the buffer is *now*.
    ///
    /// A row shorter than the block contributes only what it has: there are no
    /// characters past the end of a line to take, and inventing spaces would be
    /// inventing text. `None` on the right is the end of the line, which is the
    /// whole of what is left of it.
    fn block_span_on(&self, row: usize, left: usize, right: Option<usize>) -> (usize, usize) {
        let len = self.buffer.line_len(row);
        let base = self.buffer.line_col_to_char(row, 0);
        let start = base + left.min(len);
        let end = match right {
            None => base + len,
            Some(right) => base + (right + 1).min(len),
        };
        (start, end)
    }

    /// The character ranges a blockwise selection covers, one per row, in order.
    pub fn block_spans(&self) -> Vec<(usize, usize)> {
        let Some((first, last, left, right)) = self.visual_block() else {
            return Vec::new();
        };
        (first..=last)
            .map(|row| self.block_span_on(row, left, right))
            .collect()
    }

    /// Take the blockwise selection out into the register, and say where the
    /// cursor goes: the block's top left, as a row and a column.
    ///
    /// The rows go into the register as lines, which is what `p` can put back:
    /// vim's own block register remembers the *shape* and puts it in columns, and
    /// that is a `p` of its own — this one is the text.
    ///
    /// The rows are taken in *order*, from the first down, which is what lets the
    /// whole thing fold into one history step: `History` joins a group's steps
    /// when each one starts after the last, and a block taken from the bottom up
    /// starts before it. Each row's offsets are worked out as the deletion
    /// reaches it, since every earlier deletion moved them.
    pub fn cut_block(&mut self) -> (usize, usize) {
        let Some((first, last, left, right)) = self.visual_block() else {
            return (self.cursor.row, self.cursor.col);
        };
        let spans: Vec<(usize, usize)> = (first..=last)
            .map(|row| self.block_span_on(row, left, right))
            .collect();
        let text = self.block_text(&spans);
        if !text.is_empty() {
            self.store(Register::new(text, Kind::Blockwise), false);
        }
        for row in first..=last {
            let (start, end) = self.block_span_on(row, left, right);
            if start < end {
                self.replace_range(start, end, "", Some(start));
            }
        }
        (first, left)
    }

    /// Copy the blockwise selection into the register, and say where the cursor
    /// goes, the same way.
    pub fn copy_block(&mut self) -> (usize, usize) {
        let Some((first, last, left, right)) = self.visual_block() else {
            return (self.cursor.row, self.cursor.col);
        };
        let spans: Vec<(usize, usize)> = (first..=last)
            .map(|row| self.block_span_on(row, left, right))
            .collect();
        let text = self.block_text(&spans);
        if !text.is_empty() {
            self.store(Register::new(text, Kind::Blockwise), true);
        }
        (first, left)
    }

    fn block_text(&self, spans: &[(usize, usize)]) -> String {
        let mut text = String::new();
        for (index, (start, end)) in spans.iter().enumerate() {
            if index > 0 {
                text.push('\n');
            }
            text.push_str(&self.buffer.slice_text(*start, *end));
        }
        text
    }

    /// The key after `d`, `y` or `c`: a motion, or the operator's own key
    /// again for whole lines.
    fn handle_operator_pending(&mut self, key: KeyMsg, encoded: &str) {
        let operator = self.operator.expect("only called with one pending");

        // `v`, `V` and `<C-V>` after an operator do not start a selection: they say
        // the *shape* the motion's range will be — vim's `o_v`, `o_V` and
        // `o_CTRL-V`. None of the three is a motion, so all are answered before the
        // motion lookup, and the cursor now is one corner of what follows.
        if self.pending.is_empty() {
            let forced = match encoded {
                "v" => Some(ForcedShape::Charwise),
                "V" => Some(ForcedShape::Linewise),
                "<C-v>" => Some(ForcedShape::Blockwise),
                _ => None,
            };
            if let Some(shape) = forced {
                self.forced_shape = Some((shape, self.cursor));
                return;
            }
        }

        // A find is a motion an operator can take: `df,` deletes through the
        // comma. It waits for its character just as the bare motion does.
        if self.pending.is_empty() && (self.starts_find(encoded) || self.starts_mark(encoded)) {
            return;
        }

        // `i` and `a` ask for a text object: `diw`, `ci"`, `ya(`. What follows
        // names the object, and the operator runs over the range it gives.
        if self.pending.is_empty() {
            if self.object.is_none() && (encoded == "i" || encoded == "a") {
                self.object = Some(if encoded == "i" {
                    Object::Inner
                } else {
                    Object::Around
                });
                return;
            }
            if let Some(object) = self.object {
                let (operator, _count) = self.take_operator();
                match command::object_span(self, object, encoded) {
                    Some(span) if span.start < span.end => {
                        self.apply_operator(operator, span);
                    }
                    Some(_) => {}
                    None => {
                        self.message = format!("{encoded} is not a text object");
                    }
                }
                return;
            }
        }

        // `dd`, `yy`, `cc`, `gUU`, `guu`, `g~~`: whole lines, as many as the
        // counts say.
        if encoded == operator.key() {
            let (operator, count) = self.take_operator();
            self.run_command(operator.line_command(), &[Param::Int(count as i64)]);
            return;
        }

        // A digit counts the motion: `d3w`. A `0` with no count running is the
        // motion `move_line_start`, which is what `d0` means.
        if let Some(digit) = count_digit(&key, !self.pending_count.is_empty()) {
            if self.pending_count.len() < MAX_COUNT_DIGITS {
                self.pending_count.push(digit);
            }
            return;
        }

        // Otherwise it is a motion, and motions can be multi-key (`d g g`).
        let sequence = if self.pending.is_empty() {
            encoded.to_string()
        } else {
            format!("{} {}", self.pending, encoded)
        };
        let motion = match self.lookup_normal(&sequence) {
            Lookup::Exact(binding) => match binding_motion(&binding) {
                Some(name) => name.to_string(),
                None => {
                    // A binding that is not a motion ends the operator, the way
                    // vim beeps: `d` then `i` does not enter insert mode.
                    self.message = format!("{sequence} is not a motion");
                    self.clear_pending();
                    return;
                }
            },
            Lookup::Prefix => {
                self.pending = sequence;
                return;
            }
            Lookup::None => {
                self.unmapped(encoded);
                self.clear_pending();
                return;
            }
        };

        let (operator, count) = self.take_operator();
        // `d<C-V>` — vim's `o_CTRL-V`: the operator works blockwise, and the
        // block's corners are where the cursor was and where the motion took it.
        // So the motion runs as a motion, the rectangle is made into a selection,
        // and the operator's *visual* command applies it — the same path every
        // other block operation goes through.
        if let Some((ForcedShape::Blockwise, anchor)) = self.forced_shape {
            self.run_command(&motion, &[Param::Int(count as i64)]);
            self.selection = Some(Selection {
                anchor,
                kind: VisualKind::Blockwise,
                to_end: false,
            });
            self.run_command(operator.visual_command(), &[]);
            return;
        }
        self.run_command(
            operator.command(),
            &[Param::Int(count as i64), Param::Str(motion)],
        );
    }

    /// Select what a text object named.
    ///
    /// A span of characters becomes a characterwise selection with the cursor on
    /// its last character. A span of whole lines has to become a *linewise*
    /// selection: the last character of a line is its terminator, and a
    /// normal-mode cursor cannot sit on one, so the two ends of a linewise range
    /// are lines and not characters.
    pub(crate) fn select_object(&mut self, span: crate::command::Span) {
        let mut anchor = self.cursor;
        anchor.set_char_idx(&self.buffer, span.start);
        let mut cursor = self.cursor;
        if span.linewise {
            let last = self.buffer.char_to_line_col(span.end.saturating_sub(1)).0;
            cursor.set_char_idx(&self.buffer, self.buffer.line_col_to_char(last, 0));
        } else {
            cursor.set_char_idx(&self.buffer, span.end.saturating_sub(1).max(span.start));
        }
        self.selection = Some(Selection {
            anchor,
            to_end: false,
            kind: if span.linewise {
                VisualKind::Linewise
            } else {
                VisualKind::Charwise
            },
        });
        self.cursor = cursor;
        self.mode = Mode::Visual;
        self.after_edit();
    }

    /// Whether this key starts a character find, which then waits for the one
    /// key that names its character.
    ///
    /// A find is two keys and the second is any key at all, so it cannot be a
    /// keymap binding: the keymap is a table of names, and the character is not
    /// a name.
    fn starts_find(&mut self, encoded: &str) -> bool {
        let name = match encoded {
            "f" => "find_forward",
            "F" => "find_backward",
            "t" => "till_forward",
            "T" => "till_backward",
            _ => return false,
        };
        // Only from a clean slate: after another pending key (`z` before `t`)
        // this key belongs to that sequence, not to a find of its own.
        if !self.pending.is_empty() {
            return false;
        }
        self.pending = name.to_string();
        true
    }

    /// Whether `q` or `@` is waiting for a register letter, and what to do with
    /// the key that arrived.
    ///
    /// Returns whether the key was taken. `q` and `@` are *not* a second pending
    /// mechanism beside the keymap's: they are what the keymap's pending state
    /// means when the key that follows is one the table cannot name, because a
    /// register letter is any letter. The one exception is the pair the table
    /// *does* name — `q:` and `q/` — which is handed back to it.
    fn take_macro_letter(&mut self, encoded: &str) -> bool {
        let Some(ask) = self.macro_ask else {
            return false;
        };
        // `@@` plays the macro `@` was last given, which is the whole point of
        // remembering it.
        if ask == MacroAsk::Play && encoded == "@" {
            self.macro_ask = None;
            let count = self.take_count();
            match self.last_macro {
                Some(letter) => self.play_macro(letter, count),
                None => self.message = "E748: No previous macro".to_string(),
            }
            return true;
        }
        let letter = encoded
            .chars()
            .next()
            .filter(|c| c.is_ascii_lowercase() && encoded.chars().count() == 1);
        if letter.is_none() {
            // `q:` and `q/` are the history windows, and the keymap knows them.
            if ask == MacroAsk::Record && matches!(encoded, ":" | "/") {
                return false;
            }
            self.macro_ask = None;
            return true;
        }
        let letter = letter.expect("just checked");
        self.macro_ask = None;
        match ask {
            MacroAsk::Record => {
                if let Some((recording, keys)) = self.recording.take() {
                    // A `q` while recording ends it — whichever letter came with
                    // it, since the register is the one being recorded into. The
                    // `q` itself is not part of the macro: it was pushed on the way
                    // in, so it comes off.
                    let keys = keys.strip_suffix('q').unwrap_or(&keys).to_string();
                    let index = (recording as u8).wrapping_sub(b'a') as usize;
                    self.macros[index] = Some(keys);
                    self.message = format!("recorded @{recording}");
                    return true;
                }
                self.recording = Some((letter, String::new()));
                self.message = format!("recording @{letter}");
            }
            // A count before `@` is how many times: `3@a`.
            MacroAsk::Play => {
                let count = self.take_count();
                self.play_macro(letter, count);
            }
        }
        true
    }

    /// Play a macro, `count` times.
    ///
    /// The keys go back through [`Editor::handle_key`], which is what makes a
    /// macro a macro: it is the same state machine every key goes through. The
    /// replay is flagged, so a macro cannot record itself.
    fn play_macro(&mut self, letter: char, count: usize) {
        let index = (letter as u8).wrapping_sub(b'a') as usize;
        let Some(keys) = self.macros.get(index).and_then(|held| held.clone()) else {
            self.message = format!("E748: No such macro: {letter}");
            return;
        };
        self.last_macro = Some(letter);
        let parsed = crate::keymap::parse_key(&keys).unwrap_or_default();
        self.replaying = true;
        for _ in 0..count {
            for key in &parsed {
                self.handle_key(key.clone());
            }
        }
        self.replaying = false;
    }

    /// Whether a key is one of the three that wait for a mark's letter, and what
    /// it will do with it.
    ///
    /// Like `f{char}`, these are keys and not commands: there is no letter to
    /// give a command until the next key arrives.
    fn starts_mark(&mut self, encoded: &str) -> bool {
        let jump = match encoded {
            "m" => MarkJump::Set,
            "`" => MarkJump::Exact,
            "'" => MarkJump::Line,
            _ => return false,
        };
        // Only from a clean slate: a `z` before a `t` belongs to the `zt`.
        if !self.pending.is_empty() {
            return false;
        }
        self.mark_prefix = Some(jump);
        true
    }

    /// The letter of a mark that is waiting for one.
    ///
    /// Any letter does, and anything else calls it off the way vim does. The
    /// three keys are told apart by what they do with the letter: `m` puts a
    /// mark down, and the two jumps run the motion, over an operator if one is
    /// waiting — which is what makes `d'a` take the lines and `d`a` the text up
    /// to the mark.
    fn take_mark_target(&mut self, encoded: &str) {
        let Some(jump) = self.mark_prefix.take() else {
            return;
        };
        let mut chars = encoded.chars();
        let Some(letter) = chars.next().filter(|_| chars.next().is_none()) else {
            self.clear_pending();
            return;
        };
        // `''` and `` `` `` are the last jump rather than a mark, which is why
        // the key is taken here and not by the address parser.
        if letter == '\'' || letter == '`' {
            let name = if jump == MarkJump::Line {
                "jump_back_line"
            } else {
                "jump_back_exact"
            };
            if self.operator.is_some() {
                let (operator, count) = self.take_operator();
                self.run_command(
                    operator.command(),
                    &[Param::Int(count as i64), Param::Str(name.to_string())],
                );
                return;
            }
            let count = self.take_count();
            let _ = count;
            self.pending.clear();
            self.run_command(name, &[]);
            return;
        }
        if !letter.is_ascii_lowercase() {
            self.clear_pending();
            return;
        }
        self.last_mark = Some((letter, jump));
        let params = [Param::Str(letter.to_string())];
        if jump == MarkJump::Set {
            let count = self.take_count();
            let _ = count;
            self.run_command("set_mark", &params);
            self.pending.clear();
            return;
        }
        let name = match jump {
            MarkJump::Exact => "mark_exact",
            _ => "mark_line",
        };
        match self.operator {
            Some(_) => {
                let (operator, count) = self.take_operator();
                self.run_command(
                    operator.command(),
                    &[Param::Int(count as i64), Param::Str(name.to_string())],
                );
            }
            None => {
                let count = self.take_count();
                self.pending.clear();
                self.run_command(
                    name,
                    &[Param::Int(count as i64), Param::Str(letter.to_string())],
                );
            }
        }
    }

    /// The key that names the character of a find that is waiting for one.
    ///
    /// `Escape` is not a character: it calls the find off, the way vim does.
    fn take_find_target(&mut self, encoded: &str, name: &str) -> bool {
        if encoded == "<Esc>" {
            self.clear_pending();
            return true;
        }
        let mut chars = encoded.chars();
        let Some(target) = chars.next().filter(|_| chars.next().is_none()) else {
            self.clear_pending();
            return true;
        };
        // The find remembers its character here, before anything runs: the
        // motion reads it from there, and so does the operator path, which is
        // handed the motion's *name* and not its character.
        if let Some(kind) = command::find_of(name) {
            self.last_find = Some((target, kind));
        }
        match self.operator {
            Some(_) => {
                let (operator, count) = self.take_operator();
                self.run_command(
                    operator.command(),
                    &[Param::Int(count as i64), Param::Str(name.to_string())],
                );
            }
            None => {
                let count = self.take_count();
                self.pending.clear();
                self.run_command(
                    name,
                    &[Param::Int(count as i64), Param::Str(target.to_string())],
                );
            }
        }
        true
    }

    /// The pending operator and its count, spent.
    ///
    /// The count is the operator's times the motion's, which is what makes
    /// `2d3w` six words without either of them knowing about the other.
    fn take_operator(&mut self) -> (Operator, usize) {
        let operator = self.operator.expect("only called with one pending");
        let count = self.operator_count * self.take_count();
        self.operator = None;
        self.object = None;
        self.operator_count = 1;
        self.pending.clear();
        (operator, count)
    }

    /// Run a pending operator over a range a text object named.
    ///
    /// Most objects are a stretch of a line, and a paragraph is whole lines: the
    /// span says which, so the register that `dip` fills pastes as lines the way
    /// `dd` does.
    fn apply_operator(&mut self, operator: Operator, span: crate::command::Span) {
        // A closed fold is a whole, as the manual puts it: an operator that reaches
        // into one takes all of it — "`dl` deletes the whole closed fold under the
        // cursor". The range is widened to the fold's own lines before anything is
        // done with it.
        let span = self.widen_over_folds(span);
        let kind = if span.linewise {
            Kind::Linewise
        } else {
            Kind::Charwise
        };
        let (start, end) = (span.start, span.end);
        match operator {
            Operator::Fold => {
                // A fold is a range of *lines*, whatever the motion's shape was.
                let first = self.buffer.char_to_line_col(start).0;
                let last = self.buffer.char_to_line_col(end.saturating_sub(1)).0;
                self.fold_lines(first, last);
            }
            Operator::Delete => self.cut(start, end, kind, Some(start)),
            Operator::Yank => {
                self.copy(start, end, kind);
                // `yiw` leaves the cursor where the object begins.
                self.cursor.set_char_idx(&self.buffer, start);
                self.after_edit();
            }
            Operator::Change => {
                // Insert mode first: the cut ends with the cursor clamped to a
                // real character for normal mode, and the text about to be typed
                // belongs where the object began — which for a word at the end
                // of a line is one past the last character.
                self.mode = Mode::Insert;
                self.cut(start, end, kind, Some(start));
                self.cursor.set_char_idx(&self.buffer, start);
            }
            // A case change is not a cut: the text stays where it is, and the
            // register keeps what it had. The object's range is characters, so
            // the linewise flag only ever says how `d` and `y` should treat what
            // they take.
            Operator::ToggleCase => {
                self.change_case(start, end, crate::command::CaseChange::Toggle)
            }
            // A rewrap is about lines, so it takes the whole of them however the
            // range was named — an object gives a run of characters, and the
            // lines those characters are on are what is rewritten.
            Operator::Format => {
                let from = self.buffer.char_to_line_col(start).0;
                let to = self.buffer.char_to_line_col(end.saturating_sub(1)).0;
                self.format_lines(from, to);
            }
            Operator::Reindent => {
                let from = self.buffer.char_to_line_col(start).0;
                let to = self.buffer.char_to_line_col(end.saturating_sub(1)).0;
                self.reindent_lines(from, to);
            }
            Operator::Indent => {
                let from = self.buffer.char_to_line_col(start).0;
                let to = self.buffer.char_to_line_col(end.saturating_sub(1)).0;
                self.shift_indent(from, to, true);
            }
            Operator::Outdent => {
                let from = self.buffer.char_to_line_col(start).0;
                let to = self.buffer.char_to_line_col(end.saturating_sub(1)).0;
                self.shift_indent(from, to, false);
            }
            Operator::Lower => self.change_case(start, end, crate::command::CaseChange::Lower),
            Operator::Upper => self.change_case(start, end, crate::command::CaseChange::Upper),
        }
    }

    /// Rewrap lines to `textwidth`, as one edit.
    ///
    /// The lines are joined and filled again, which is what vim's `gq` does
    /// without its comment handling: a word that is longer than the width is left
    /// on a line of its own rather than broken, and `textwidth = 0` is not a
    /// width at all — the command then does nothing, which is what "do not wrap"
    /// has to mean.
    pub fn format_lines(&mut self, from: usize, to: usize) -> usize {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        let (from, to) = (from.min(to), from.max(to));
        let width = self.options.borrow().textwidth.0;
        // What every line of the range has in front of it is what each line of
        // the rewrap gets in front of it: a paragraph of `// …` stays a
        // paragraph of `// …`, and a person reads what comes after the marker,
        // so the marker does not count towards the width.
        let leader = self.common_leader(from, to);
        let mut words: Vec<String> = Vec::new();
        for row in from..=to {
            let line = self.buffer.line_string(row);
            let rest = line.strip_prefix(leader.as_str()).unwrap_or(&line);
            words.extend(rest.split_whitespace().map(str::to_string));
        }
        if words.is_empty() || width == 0 {
            return to - from + 1;
        }
        let width = width.saturating_sub(leader.chars().count()).max(1);
        let mut lines: Vec<String> = Vec::new();
        let mut current = String::new();
        for word in words {
            if current.is_empty() {
                current = word;
                continue;
            }
            if current.chars().count() + 1 + word.chars().count() > width {
                lines.push(std::mem::take(&mut current));
                current = word;
            } else {
                current.push(' ');
                current.push_str(&word);
            }
        }
        lines.push(current);
        // A list keeps its marker on the first line and lines the rest up under
        // the text: `- one two` then spaces, not `- one two` then `- three`. The
        // marker is a bullet or a number, and what follows it is spaces of the
        // same width — which is what a person writing prose means by a list.
        // The marker may be in the shared leader, or — for a range of one line,
        // where nothing is shared — at the start of the line itself.
        let marker = Self::list_marker_width(&leader)
            .or_else(|| Self::list_marker_width(&self.buffer.line_string(from)));
        let continuation = match marker {
            Some(width) => " ".repeat(width),
            None => leader.clone(),
        };
        let lines: Vec<String> = lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                if index == 0 {
                    format!("{leader}{line}")
                } else {
                    format!("{continuation}{line}")
                }
            })
            .collect();
        let mut text = lines.join("\n");
        // The line break the range ended on belongs to the text after it, not to
        // the last line: a range that stopped before one does not gain one.
        let terminator = self.buffer.line_terminator(to);
        text.push_str(&terminator);
        let start = self.buffer.line_col_to_char(from, 0);
        let end = self.buffer.line_col_to_char(to, 0)
            + self.buffer.line_len(to)
            + self.buffer.line_terminator_len(to);
        let count = lines.len();
        self.replace_range(start, end, &text, Some(start));
        count
    }

    /// How wide a list marker is, if the leader is one: `- `, `* `, `+ `, `1. `.
    ///
    /// What a wrapped list line is indented by is the marker's *width*, since the
    /// marker belongs to the item and not to every line of it.
    fn list_marker_width(leader: &str) -> Option<usize> {
        let trimmed = leader.trim_start();
        let indent = leader.chars().count() - trimmed.chars().count();
        let mut chars = trimmed.chars();
        match chars.next()? {
            '-' | '*' | '+' => chars.next().filter(|c| *c == ' ').map(|_| indent + 2),
            first if first.is_ascii_digit() => {
                let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
                let after = trimmed.chars().nth(digits)?;
                let space = trimmed.chars().nth(digits + 1)?;
                (matches!(after, '.' | ')') && space == ' ').then_some(indent + digits + 2)
            }
            _ => None,
        }
    }

    /// What every line of a range has in front of it, and nothing when they do
    /// not agree.
    ///
    /// The leader is the longest run of spaces, markers and markers' spaces that
    /// all the lines share — `// `, `# `, `> `, or plain indentation — trimmed
    /// back to a space so that a leader cannot end in the middle of a word.
    fn common_leader(&self, from: usize, to: usize) -> String {
        let lines: Vec<String> = (from..=to)
            .map(|row| self.buffer.line_string(row))
            .collect();
        let first = match lines.first() {
            Some(line) => line,
            None => return String::new(),
        };
        let mut len = first.chars().count();
        for line in &lines {
            let common = first
                .chars()
                .zip(line.chars())
                .take_while(|(a, b)| a == b && (*a == ' ' || !a.is_alphanumeric()))
                .count();
            len = len.min(common);
        }
        if len == 0 {
            return String::new();
        }
        let leader: String = first.chars().take(len).collect();
        // A leader that ends in a marker's space keeps it; one that ends in the
        // middle of anything else is cut back to its last space.
        if leader.ends_with(' ') {
            return leader;
        }
        match leader.rfind(' ') {
            Some(at) => leader[..=at].to_string(),
            None if lines.len() > 1 => String::new(),
            None => leader,
        }
    }

    /// Change the case of a range, in place.
    ///
    /// The register is untouched, and the cursor goes to the start of the range,
    /// which is where an operator leaves it.
    pub fn change_case(&mut self, start: usize, end: usize, change: crate::command::CaseChange) {
        if start >= end {
            return;
        }
        let text: String = self
            .buffer
            .slice_text(start, end)
            .chars()
            .map(|c| match change {
                crate::command::CaseChange::Toggle if c.is_uppercase() => {
                    c.to_lowercase().next().unwrap_or(c)
                }
                crate::command::CaseChange::Toggle if c.is_lowercase() => {
                    c.to_uppercase().next().unwrap_or(c)
                }
                crate::command::CaseChange::Lower => c.to_lowercase().next().unwrap_or(c),
                crate::command::CaseChange::Upper => c.to_uppercase().next().unwrap_or(c),
                crate::command::CaseChange::Toggle => c,
            })
            .collect();
        self.replace_range(start, end, &text, Some(start));
        self.cursor.set_char_idx(&self.buffer, start);
    }

    /// Forget a half-typed command: the pending keys, the count and the
    /// operator. Called whenever the mode changes, so `d` then Escape leaves
    /// nothing behind.
    pub fn clear_pending(&mut self) {
        // What a mode was doing is remembered here, where a mode *stops*: the
        // last selection is what `gv` comes back to, and the last place insert
        // mode was is what `gi` does. Both are per document, and both are read
        // before the mode changes rather than after.
        if self.mode == Mode::Visual {
            if let Some(selection) = self.selection {
                self.last_visual = Some((selection, self.cursor));
            }
        }
        if self.mode == Mode::Insert {
            self.insert_again_at = Some(self.cursor);
        }
        self.pending.clear();
        self.insert_pending.clear();
        self.pending_count.clear();
        self.operator = None;
        self.object = None;
        self.mark_prefix = None;
        self.register_prefix = Prefix::None;
        self.operator_count = 1;
        if self.mode != Mode::Visual {
            self.selection = None;
        }
    }

    /// The count the user typed, and the count is spent.
    fn take_count(&mut self) -> usize {
        let count = self
            .pending_count
            .parse::<usize>()
            .ok()
            .filter(|count| *count > 0)
            .unwrap_or(1);
        self.pending_count.clear();
        count
    }

    fn lookup_normal(&self, sequence: &str) -> Lookup {
        let keymap = self.keymap.borrow();
        keymap.lookup(Mode::Normal, sequence)
    }

    fn run_binding(&mut self, binding: Binding, count: usize) {
        let params = count_params(count);
        for name in binding.commands {
            self.run_command(&name, &params);
            if self.should_quit {
                break;
            }
        }
    }

    fn unmapped(&mut self, encoded: &str) {
        self.message = format!("unmapped key: {encoded}");
    }

    /// Start a blockwise insert: the same typing on every row of a rectangle.
    ///
    /// `col` is where the typing starts on each row — the block's left column for
    /// `I` and `c`, one past its right one for `A`. A row too short to reach the
    /// column is *padded* with spaces, the way the blockwise put pads: the text
    /// is meant to line up in columns, and a column past the end of a line is a
    /// place that has to exist first.
    /// `rows` are the rows the typing goes on — the caller decides which of the
    /// block's rows are in it, since that depends on the block's *shape* and not
    /// on what the buffer looks like after a deletion. `col` is where it starts on
    /// each, or `None` for the end of the line, and `pad` says whether a row that
    /// does not reach `col` is padded out to it (which is what `A` does, and what
    /// `I` and `c` do not: `v_b_I` leaves a short line unmodified).
    pub fn start_block_insert(&mut self, rows: Vec<usize>, col: Option<usize>, pad: bool) {
        let mut cursors = Vec::new();
        for row in rows {
            let len = self.buffer.line_len(row);
            // `None` is the end of each line: the typing goes after whatever the
            // line has, so there is nothing to pad.
            let col = match col {
                Some(col) => col,
                None => len,
            };
            if col > len && pad {
                let at = self.buffer.line_col_to_char(row, 0) + len;
                let fill = " ".repeat(col - len);
                self.replace_range(at, at, &fill, Some(at + fill.chars().count()));
            }
            cursors.push(Cursor::new(row, col.min(self.buffer.line_len(row))));
        }
        if cursors.is_empty() {
            return;
        }
        self.cursor = cursors[0];
        self.block_insert = Some((cursors, 0));
        self.clear_pending();
        self.mode = Mode::Insert;
    }

    /// Apply the key that was just handled to every other row of a blockwise
    /// insert, which is what makes `I` on a rectangle type on all of it.
    ///
    /// Each row keeps its own position, since the rows are different lengths and
    /// what is typed on one of them is not where the others are. A key that
    /// leaves insert mode leaves it for all of them, and the session is over.
    fn replay_block_insert(&mut self, key: KeyMsg) {
        let Some((rows, current_index)) = self.block_insert.take() else {
            return;
        };
        if self.mode != Mode::Insert {
            return;
        }
        let typed_on = self.cursor;
        let mut updated = rows;
        for (index, position) in updated.iter_mut().enumerate() {
            if index == current_index {
                *position = typed_on;
                continue;
            }
            self.cursor = *position;
            self.handle_insert(key.clone());
            *position = self.cursor;
            if self.mode != Mode::Insert {
                // Something in the key ended the session; the rows that have not
                // been reached are simply not typed on.
                self.cursor = typed_on;
                self.redraw = true;
                return;
            }
        }
        self.cursor = typed_on;
        self.block_insert = Some((updated, current_index));
        self.redraw = true;
    }

    fn handle_insert(&mut self, key: KeyMsg) {
        let encoded = encode_key(&key);

        // A sequence that is already part-way typed is either a mapping or
        // literal text: `jk` mapped to `<Esc>` means the `j` was not typing,
        // and `jx` means it was, after all.
        if !self.insert_pending.is_empty() {
            let sequence = format!("{} {}", self.pending_insert_keys(), encoded);
            match self.lookup_insert(&sequence) {
                Lookup::Exact(binding) => {
                    self.insert_pending.clear();
                    self.run_binding(binding, 1);
                    return;
                }
                Lookup::Prefix => {
                    self.insert_pending.push(key);
                    return;
                }
                Lookup::None => self.flush_insert_pending(),
            }
        }

        // An explicit insert-mode mapping — one key, or the start of a longer
        // one — wins over typing the character.
        match self.lookup_insert(&encoded) {
            Lookup::Exact(binding) => {
                self.run_binding(binding, 1);
                return;
            }
            Lookup::Prefix => {
                self.insert_pending.push(key);
                return;
            }
            Lookup::None => {}
        }

        if key.is_ctrl() || key.is_alt() {
            return;
        }
        match key.code {
            KeyCode::Esc => self.run_command("escape", &[]),
            KeyCode::Enter => self.type_text("\n"),
            KeyCode::Tab => self.type_text("\t"),
            KeyCode::Backspace => self.delete_backward(),
            KeyCode::Char(c) => self.type_text(&c.to_string()),
            // Everything else (arrows, Home/End/Delete, ...) is an ordinary
            // insert-mode binding and was handled by the lookup above.
            _ => {}
        }
    }

    fn lookup_insert(&self, sequence: &str) -> Lookup {
        let keymap = self.keymap.borrow();
        keymap.lookup(Mode::Insert, sequence)
    }

    /// The keys held back so far, as the notation a binding is looked up by.
    fn pending_insert_keys(&self) -> String {
        self.insert_pending
            .iter()
            .map(encode_key)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Type the held-back keys after all, in the order they were pressed.
    ///
    /// They are typed the way an unmapped press is — a character is a
    /// character, and a `<BS>` deletes — rather than looked up again, since they
    /// have already been shown not to be a mapping between them.
    fn flush_insert_pending(&mut self) {
        let keys = std::mem::take(&mut self.insert_pending);
        for key in keys {
            match key.code {
                KeyCode::Char(c) if !key.is_ctrl() && !key.is_alt() => {
                    self.type_text(&c.to_string())
                }
                KeyCode::Enter => self.type_text("\n"),
                KeyCode::Tab => self.type_text("\t"),
                KeyCode::Backspace => self.delete_backward(),
                KeyCode::Esc => self.run_command("escape", &[]),
                _ => {}
            }
        }
    }

    /// Insert `text` at the cursor, as one undoable change.
    ///
    /// This is what a character typed in insert mode does, and what a paste or
    /// an input method's commit does through `ClientMsg::Text`: the cursor ends
    /// after the text, and line endings are kept exactly as they came.
    pub fn type_text(&mut self, text: &str) {
        // A line break typed in insert mode starts the new line where the last
        // one did, when `autoindent` says so. It is part of the same edit as the
        // break, so one `u` undoes both.
        // A `}` typed where a line has nothing but indentation gives a level
        // back, which is the other half of `smartindent`.
        if text == "}" && self.mode == Mode::Insert && self.options.borrow().smartindent {
            let row = self.cursor.row;
            let indent = self.indent_of(row);
            let col = self.cursor.col;
            if col == indent.chars().count() && !indent.is_empty() {
                self.outdent_current_line();
            }
        }
        if text == "\n" && self.mode == Mode::Insert && self.indents_new_lines() {
            let indent = self.indent_for_new_line(self.cursor.row);
            let typed = format!("\n{indent}");
            let idx = self.cursor.char_idx(&self.buffer);
            let end = idx + typed.chars().count();
            self.replace_range(idx, idx, &typed, Some(end));
            self.wrap_typed_line(end);
            self.log_inserted(&typed);
            if self.completion.is_some() {
                self.complete(None);
            }
            return;
        }
        // A selected placeholder is *replaced* by what is typed: that is what
        // selecting it was for, and it is one edit rather than a delete and an
        // insert, so it is one thing to undo.
        if let Some((sel_start, sel_end)) = self.selected_range() {
            self.selection = None;
            let after = sel_start + text.chars().count();
            self.replace_range(sel_start, sel_end, text, Some(after));
            self.log_inserted(text);
            if self.completion.is_some() {
                self.complete(None);
            }
            return;
        }
        let idx = self.cursor.char_idx(&self.buffer);
        let end = idx + text.chars().count();
        self.replace_range(idx, idx, text, Some(end));
        self.wrap_typed_line(end);
        self.log_inserted(text);
        if self.completion.is_some() {
            self.complete(None);
        }
    }

    /// The indentation a new line should start with, after `row`.
    ///
    /// `autoindent` copies the line; `smartindent` copies it and adds a level
    /// when the line opens something — `{`, `(`, `[` — since the line after an
    /// opener is inside it. The level is a tab, which is what the Tab key types,
    /// so the file keeps one convention rather than two.
    pub fn indent_for_new_line(&self, row: usize) -> String {
        let options = self.options.borrow();
        let smart = options.smartindent;
        if !options.autoindent && !smart {
            return String::new();
        }
        drop(options);
        let mut indent = self.indent_of(row);
        if smart && self.line_opens_something(row) {
            indent.push('\t');
        }
        indent
    }

    /// Whether a line ends by opening something, which is what the line after it
    /// is inside of.
    fn line_opens_something(&self, row: usize) -> bool {
        self.buffer
            .line_string(row)
            .trim_end()
            .chars()
            .next_back()
            .is_some_and(|c| c == '{' || c == '(' || c == '[')
    }

    /// One level of indentation, taken off the front of the line the cursor is
    /// on — what typing a `}` at the start of a line does.
    fn outdent_current_line(&mut self) {
        let row = self.cursor.row;
        let indent = self.indent_of(row);
        if indent.is_empty() {
            return;
        }
        let start = self.buffer.line_col_to_char(row, 0);
        let mut end = start;
        while end < self.buffer.len_chars()
            && matches!(self.buffer.char_at(end), Some(' ') | Some('\t'))
        {
            end += 1;
        }
        let level = if indent.ends_with('\t') {
            end - 1
        } else {
            // Spaces: one tab stop's worth, or all of them if there are fewer.
            let stop = self.options.borrow().tabstop;
            (start + stop).min(end)
        };
        let at = self.cursor.char_idx(&self.buffer);
        self.replace_range(level, end, "", Some(at.saturating_sub(end - level)));
    }

    /// Move a range of lines one level in or out, as one edit.
    ///
    /// A level is a tab, since that is what the Tab key types; taking one *off*
    /// removes a tab if the line starts with one, and otherwise as many spaces as
    /// a tab stop is worth — or all of them, when there are fewer, so a line that
    /// is barely indented is not left with a level's worth of nothing. A blank
    /// line is left blank: indenting nothing is not indentation.
    pub fn shift_indent(&mut self, from: usize, to: usize, forward: bool) -> usize {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        let (from, to) = (from.min(to), from.max(to));
        let stop = self.options.borrow().tabstop;
        let mut text = String::new();
        let mut count = 0;
        for row in from..=to {
            let line = self.buffer.line_string(row);
            if line.trim().is_empty() {
                text.push_str(&self.buffer.line_terminator(row));
                count += 1;
                continue;
            }
            if forward {
                text.push('\t');
                text.push_str(&line);
            } else {
                let indent = self.indent_of(row);
                let remove = if indent.ends_with('\t') {
                    1
                } else {
                    stop.min(indent.chars().count())
                };

                text.push_str(&line.chars().skip(remove).collect::<String>());
            }
            text.push_str(&self.buffer.line_terminator(row));
            count += 1;
        }
        let start = self.buffer.line_col_to_char(from, 0);
        let end = self.buffer.line_col_to_char(to, 0)
            + self.buffer.line_len(to)
            + self.buffer.line_terminator_len(to);
        let at = self.cursor.char_idx(&self.buffer);
        let caret = at.saturating_sub(if forward { 0 } else { at - start });
        self.replace_range(start, end, &text, Some(caret.max(start)));
        count
    }

    /// Apply a list of edits to the document, as one step to undo.
    ///
    /// The edits are given in the *original* text's character offsets — which is
    /// what a server's answer means — so each one is moved by what the ones before
    /// it changed: they are applied in document order, with a running difference,
    /// which is also the order the history records them in. Applying them
    /// backwards would need no arithmetic and would record the steps backwards,
    /// and a group of steps that runs backwards is a group that cannot be folded
    /// into one — which is what makes one `u` take the whole answer back.
    pub fn apply_edits(&mut self, edits: &[(usize, usize, String)]) -> usize {
        if edits.is_empty() {
            return 0;
        }
        let mut ordered: Vec<&(usize, usize, String)> = edits.iter().collect();
        ordered.sort_by_key(|(start, _, _)| *start);
        let len = self.buffer.len_chars();
        self.history.begin_group();
        let mut applied = 0;
        let mut delta = 0isize;
        for (start, end, text) in ordered {
            let (start, end) = ((*start).min(len), (*end).min(len));
            if start > end {
                continue;
            }
            let at = (start as isize + delta).max(0) as usize;
            let to = (end as isize + delta).max(0) as usize;
            self.replace_range(at, to, text, None);
            delta += text.chars().count() as isize - (end - start) as isize;
            applied += 1;
        }
        self.history.end_group(&mut self.buffer);
        self.cursor
            .set_char_idx(&self.buffer, self.cursor.char_idx(&self.buffer).min(len));
        self.clamp_normal_cursor();
        applied
    }

    /// Reindent a range by what its lines open and close, as one edit.
    ///
    /// The depth is counted from the beginning of the *document*, not from the
    /// start of the range: a block in the middle of a file is inside everything
    /// above it, and reindenting it should not forget that. Brackets are counted
    /// as they are written, without knowing what is a string and what is code —
    /// the highlighter knows, and this deliberately does not ask it, because a
    /// reindent that is wrong about a brace inside a string is a reindent a person
    /// can fix by hand.
    pub fn reindent_lines(&mut self, from: usize, to: usize) -> usize {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        let (from, to) = (from.min(to), from.max(to));
        // The depth the first line starts at.
        let mut depth = self.depth_before(from);
        let mut text = String::new();
        let mut count = 0;
        for row in from..=to {
            let line = self.buffer.line_string(row);
            let trimmed = line.trim_start();
            // A line that closes something is written one level out from the
            // line before it, which is what makes `}` line up with its `{`.
            let closes =
                trimmed.starts_with('}') || trimmed.starts_with(')') || trimmed.starts_with(']');
            let level = if closes {
                depth.saturating_sub(1)
            } else {
                depth
            };
            if trimmed.is_empty() {
                // An empty line stays empty: trailing whitespace is not
                // indentation.
                text.push_str(&self.buffer.line_terminator(row));
            } else {
                text.push_str(&"\t".repeat(level));
                text.push_str(trimmed);
                text.push_str(&self.buffer.line_terminator(row));
            }
            depth = level + self.line_delta(&line);
            count += 1;
        }
        let start = self.buffer.line_col_to_char(from, 0);
        let end = self.buffer.line_col_to_char(to, 0)
            + self.buffer.line_len(to)
            + self.buffer.line_terminator_len(to);
        self.replace_range(start, end, &text, Some(start));
        count
    }

    /// How deep the brackets are before a line, counting from the top of the
    /// document.
    fn depth_before(&self, row: usize) -> usize {
        let mut depth = 0usize;
        for earlier in 0..row {
            depth = depth
                .saturating_add_signed(self.line_delta_signed(&self.buffer.line_string(earlier)));
        }
        depth
    }

    /// How much one line opens or closes: `{` is one deeper, `}` one shallower.
    fn line_delta(&self, line: &str) -> usize {
        self.line_delta_signed(line).max(0) as usize
    }

    fn line_delta_signed(&self, line: &str) -> isize {
        let mut delta = 0isize;
        for c in line.chars() {
            match c {
                '{' | '(' | '[' => delta += 1,
                '}' | ')' | ']' => delta -= 1,
                _ => {}
            }
        }
        delta
    }

    /// The indentation of a line: its leading spaces and tabs.
    pub fn indent_of(&self, row: usize) -> String {
        let line = self.buffer.line_string(row);
        line.chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect()
    }

    /// Whether a new line is indented at all: `autoindent` or `smartindent`.
    fn indents_new_lines(&self) -> bool {
        let options = self.options.borrow();
        options.autoindent || options.smartindent
    }

    /// Break the line that was just typed into, if it has grown past
    /// `textwidth`.
    ///
    /// This is what makes `textwidth` a setting rather than a command: a line
    /// that reaches past the width is broken at the last space before it, which
    /// is the space the typist just passed. A word longer than the width is not
    /// broken — there is no space to break at — and the new line is not indented,
    /// since indentation is a thing this editor does not do while typing.
    fn wrap_typed_line(&mut self, at: usize) {
        let width = self.options.borrow().textwidth.0;
        if width == 0 {
            return;
        }
        let (row, col) = self.buffer.char_to_line_col(at);
        if self.buffer.line_len(row) <= width {
            return;
        }
        let line_start = self.buffer.line_col_to_char(row, 0);
        let len = self.buffer.line_len(row);
        let _ = col;
        // The last space *before the width*, not before the cursor: a break
        // there does not depend on when the line was noticed, which is what
        // makes the result the same whether a character or a paste arrived.
        let space = (0..len.min(width)).rev().find(|index| {
            self.buffer
                .char_at(line_start + index)
                .is_some_and(|c| c == ' ')
        });
        let Some(space) = space else {
            return;
        };
        let break_at = line_start + space;
        // The cursor does not move: the break takes the place of a space the
        // typist has already passed, so what comes next still belongs where it
        // did — at the end of the text, which is now the end of the next line.
        self.replace_range(break_at, break_at + 1, "\n", Some(at));
    }

    /// The range a selection covers, in characters, if one is up.
    fn selected_range(&self) -> Option<(usize, usize)> {
        let selection = self.selection?;
        let anchor = selection.anchor.char_idx(&self.buffer);
        let cursor = self.cursor.char_idx(&self.buffer);
        let (start, end) = if anchor <= cursor {
            (anchor, cursor + 1)
        } else {
            (cursor, anchor + 1)
        };
        (start < end).then_some((start, end))
    }

    /// Select the text of a stop, so that typing into it replaces what is there.
    fn select_stop(&mut self, stop: usize) {
        let Some(span) = self.snippet_stops.get(stop).map(|stop| stop.span) else {
            return;
        };
        let (start, end) = span;
        let (start, end) = (
            start.min(self.buffer.len_chars()),
            end.min(self.buffer.len_chars()),
        );
        if end <= start {
            // A place with nothing in it is a place to type, not to replace.
            self.selection = None;
            self.cursor.set_char_idx(&self.buffer, start);
            return;
        }
        let mut anchor = self.cursor;
        anchor.set_char_idx(&self.buffer, start);
        let mut cursor = self.cursor;
        cursor.set_char_idx(&self.buffer, end - 1);
        self.selection = Some(Selection {
            anchor,
            kind: VisualKind::Charwise,
            to_end: false,
        });
        self.cursor = cursor;
    }

    /// Note text as part of the insert session that is open, if one is.
    fn log_inserted(&mut self, text: &str) {
        if self.mode != Mode::Insert {
            return;
        }
        if let Some(log) = self.insert_log.as_mut() {
            log.push_str(text);
        }
    }

    fn delete_backward(&mut self) {
        let idx = self.cursor.char_idx(&self.buffer);
        if idx == 0 {
            return;
        }
        // A backspace is part of the session's text as a token of its own:
        // pressing it twice must delete twice when `.` types it all again.
        self.log_inserted("\u{8}");
        // At the start of a line, backspace joins it with the line above, and
        // the join takes the whole terminator with it -- otherwise a CRLF file
        // would keep a stray carriage return in the middle of the joined line.
        let start = if self.cursor.col == 0 && self.cursor.row > 0 {
            idx - self.buffer.line_terminator_len(self.cursor.row - 1).max(1)
        } else {
            self.buffer.prev_grapheme_boundary(idx)
        };
        self.replace_range(start, idx, "", Some(start));
    }

    fn handle_command_line(&mut self, key: KeyMsg) {
        match key.code {
            KeyCode::Esc => {
                // A cancelled search leaves the cursor where it started and puts
                // back the search that was in force.
                self.end_preview(false);
                self.mode = Mode::Normal;
                self.cmdline.clear();
                self.cmdline_cursor = 0;
                self.forget_recall();
            }
            KeyCode::Enter => {
                // The preview moved the cursor to the match it found; the search
                // itself runs from where the prompt was opened, so it lands on
                // that same match rather than on the one after it.
                self.end_preview(true);
                let line = std::mem::take(&mut self.cmdline);
                self.cmdline_cursor = 0;
                self.remember_prompt(&line);
                self.forget_recall();
                let prompt = self.prompt;
                self.prompt = Prompt::Command;
                self.mode = Mode::Normal;
                match prompt.direction() {
                    Some(direction) => self.run_search(&line, direction),
                    None => self.execute_cmdline(&line),
                }
            }
            KeyCode::Backspace => {
                self.cmdline_delete_backward();
                self.preview_search();
            }
            KeyCode::Delete => {
                self.cmdline_delete_forward();
                self.preview_search();
            }
            KeyCode::Left => self.cmdline_cursor = self.cmdline_cursor.saturating_sub(1),
            KeyCode::Right => {
                self.cmdline_cursor = (self.cmdline_cursor + 1).min(self.cmdline_len())
            }
            KeyCode::Home => self.cmdline_cursor = 0,
            KeyCode::End => self.cmdline_cursor = self.cmdline_len(),
            KeyCode::Up => self.cmdline_recall(true),
            KeyCode::Down => self.cmdline_recall(false),
            // While the pattern is being typed, `<C-g>` and `<C-t>` walk the
            // matches it has so far, which is what they do in vim.
            KeyCode::Char('g') if key.is_ctrl() => self.preview_next(Direction::Forward),
            KeyCode::Char('t') if key.is_ctrl() => self.preview_next(Direction::Backward),
            KeyCode::Char(c) if !key.is_ctrl() && !key.is_alt() => {
                self.cmdline_push(c);
                self.preview_search();
            }
            _ => {}
        }
    }

    /// How long the line being typed is, in characters.
    fn cmdline_len(&self) -> usize {
        self.cmdline.chars().count()
    }

    /// Type a character at the prompt's cursor.
    fn cmdline_push(&mut self, c: char) {
        let at = byte_index(&self.cmdline, self.cmdline_cursor);
        self.cmdline.insert(at, c);
        self.cmdline_cursor += 1;
        // Typing again means the line is no longer a line from the history.
        self.prompt_index = None;
    }

    /// Delete the character before the prompt's cursor.
    fn cmdline_delete_backward(&mut self) {
        if self.cmdline_cursor == 0 {
            return;
        }
        self.cmdline_cursor -= 1;
        self.cmdline_delete_forward();
    }

    /// Delete the character at the prompt's cursor.
    fn cmdline_delete_forward(&mut self) {
        let from = byte_index(&self.cmdline, self.cmdline_cursor);
        let to = byte_index(&self.cmdline, self.cmdline_cursor + 1);
        if from == to {
            return;
        }
        self.cmdline.replace_range(from..to, "");
        self.prompt_index = None;
    }

    /// Walk the history this prompt keeps: `backwards` is `Up`.
    ///
    /// The line being typed is saved the first time and comes back when the walk
    /// runs off the end, so a half-typed command is not lost by looking at what
    /// was run before it.
    fn cmdline_recall(&mut self, backwards: bool) {
        let history: &[String] = match self.prompt {
            Prompt::Command => &self.command_history,
            _ => &self.search_history,
        };
        if history.is_empty() {
            return;
        }
        let next = match (self.prompt_index, backwards) {
            (None, true) => {
                self.prompt_draft = self.cmdline.clone();
                Some(history.len() - 1)
            }
            (None, false) => None,
            (Some(0), true) => Some(0),
            (Some(index), true) => Some(index - 1),
            (Some(index), false) => (index + 1 < history.len()).then_some(index + 1),
        };
        let line = match next {
            Some(index) => history[index].clone(),
            None => self.prompt_draft.clone(),
        };
        self.prompt_index = next;
        self.cmdline = line;
        self.cmdline_cursor = self.cmdline_len();
    }

    /// Remember a line that was run, so `Up` finds it again.
    ///
    /// Empty lines are not worth keeping, and neither is the same line twice in
    /// a row.
    fn remember_prompt(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        let history = match self.prompt {
            Prompt::Command => &mut self.command_history,
            _ => &mut self.search_history,
        };
        if history.last().map(String::as_str) != Some(line) {
            history.push(line.to_string());
        }
    }

    /// Stop reviewing the history, and forget the saved draft.
    fn forget_recall(&mut self) {
        self.prompt_index = None;
        self.prompt_draft.clear();
    }

    /// Search for `pattern`, and move the cursor to the first match after it.
    ///
    /// An empty pattern repeats the last search, which is what `/` then
    /// `<Enter>` means in vim; with nothing to repeat it says so rather than
    /// doing nothing quietly.
    pub fn run_search(&mut self, pattern: &str, direction: Direction) {
        let ignore_case = self.options.borrow().ignorecase;
        if pattern.is_empty() {
            if self.search.is_empty() {
                self.message = "E35: No previous search pattern".to_string();
                return;
            }
        } else {
            match self
                .search
                .run(&self.buffer, pattern, direction, ignore_case)
            {
                // A pattern that does not compile is not a search that found
                // nothing: the two answers are different, so they say so.
                Err(error) => {
                    self.message = format!("E54: {error}");
                    return;
                }
                Ok(0) => {
                    self.message = format!("E486: Pattern not found: {pattern}");
                    return;
                }
                Ok(_) => {}
            }
        }

        // A search from the cursor stays on a match the cursor is already in
        // (`n` is what moves on); past the last one it wraps, the way vim does.
        //
        // On a closed fold it starts *past* it, which is the manual's rule: "when
        // starting a search in a closed fold it will not find a match in the current
        // fold — a forward search always starts from the end of the closed fold, while
        // a backwards search starts from the start of the closed fold".
        let from = self.search_start(direction);
        let found = self
            .search
            .match_at(from)
            .or_else(|| self.search.next_from(from, direction));
        match found {
            Some((start, _)) if start != from => {
                // A search that moves the cursor is a jump: `''` comes back here.
                self.record_jump();
                self.goto_match(start);
            }
            Some((start, _)) => self.goto_match(start),
            None => self.message = format!("E486: Pattern not found: {}", self.search.pattern()),
        }
    }

    /// Where a search starts from: the cursor, or past a closed fold the cursor is on.
    fn search_start(&self, direction: Direction) -> usize {
        let row = self.cursor.row;
        let fold = (0..self.folds.len()).find(|index| {
            let fold = self.folds[*index];
            self.fold_is_shut(*index) && row >= fold.start && row <= fold.end
        });
        match (fold, direction) {
            (Some(index), Direction::Forward) => {
                let end = self.folds[index].end + 1;
                let last = self.buffer.line_count().saturating_sub(1);
                self.buffer.line_col_to_char(end.min(last), 0)
            }
            (Some(index), Direction::Backward) => {
                self.buffer.line_col_to_char(self.folds[index].start, 0)
            }
            _ => self.cursor.char_idx(&self.buffer),
        }
    }

    /// Move the cursor to the start of a match, and put it on screen.
    fn goto_match(&mut self, start: usize) {
        self.cursor.set_char_idx(&self.buffer, start);
        self.after_edit();
    }

    /// Show where the pattern being typed matches — `'incsearch'`.
    ///
    /// The search is run for real, so what is highlighted is the pattern being
    /// typed rather than the last one's, and the search that was in force is
    /// kept beside it for a cancellation to put back.
    ///
    /// Every keystroke searches *from where the search started* again, which is
    /// vim's rule and the reason typing more of a pattern keeps the cursor on
    /// the first match from there instead of walking through the matches.
    fn preview_search(&mut self) {
        let Some(direction) = self.prompt.direction() else {
            return;
        };
        if !self.options.borrow().incsearch {
            return;
        }
        let start = match &self.incsearch {
            Some(preview) => preview.start,
            None => {
                let previous = (!self.search.is_empty())
                    .then(|| (self.search.pattern().to_string(), self.search.direction()));
                let start = self.cursor;
                self.incsearch = Some(IncSearch {
                    start,
                    previous,
                    showing: false,
                });
                start
            }
        };
        let pattern = self.cmdline.clone();
        let ignore_case = self.options.borrow().ignorecase;
        // A pattern that does not compile, or one that matches nothing, shows
        // nothing and says nothing: this is not the search yet, and a `:` line
        // that complained on every keystroke would be unusable.
        if pattern.is_empty() {
            self.search.clear();
            self.cursor = start;
            self.showing(false);
            return;
        }
        if self
            .search
            .run(&self.buffer, &pattern, direction, ignore_case)
            .is_err()
        {
            self.search.clear();
            self.cursor = start;
            self.showing(false);
            return;
        }
        if self.search.matches().is_empty() {
            self.cursor = start;
            self.showing(false);
            return;
        }
        // The search starts where the prompt was opened, not where the last
        // preview left the cursor.
        let here = self.cursor;
        self.cursor = start;
        let from = self.search_start(direction);
        self.cursor = here;
        let found = self
            .search
            .match_at(from)
            .or_else(|| self.search.next_from(from, direction));
        match found {
            Some((at, _)) => {
                self.goto_match(at);
                self.showing(true);
            }
            None => {
                self.cursor = start;
                self.showing(false);
            }
        }
    }

    /// Move the preview on to the next match (`<C-g>`) or the previous one
    /// (`<C-t>`), while the pattern is still being typed.
    fn preview_next(&mut self, direction: Direction) {
        // `is_none_or` is newer than this workspace's MSRV; `is_some_and` is not.
        if !self
            .incsearch
            .as_ref()
            .is_some_and(|preview| preview.showing)
        {
            return;
        }
        let here = self.search.match_at(self.cursor.char_idx(&self.buffer));
        if let Some((at, _)) = here.and_then(|(at, _)| self.search.next_from(at, direction)) {
            self.goto_match(at);
        }
    }

    /// Note whether the preview has a match on screen, which is what the
    /// drawing needs to know: the cursor belongs to the match, not to the line
    /// being typed.
    fn showing(&mut self, showing: bool) {
        if let Some(preview) = self.incsearch.as_mut() {
            preview.showing = showing;
        }
    }

    /// Forget the preview of a search, if one is up — the line it belonged to is
    /// gone. The cursor is left where it is: this is for a prompt that is being
    /// opened, not one that is being cancelled.
    pub(crate) fn forget_preview(&mut self) {
        self.incsearch = None;
    }

    /// Put the cursor back where the preview's search started and forget the
    /// preview.
    ///
    /// `keep` is whether the preview's search stands: it does when the line is
    /// accepted, and does not when it is cancelled — a cancelled search puts
    /// back the one that was in force, so `n` still walks what it walked.
    fn end_preview(&mut self, keep: bool) {
        let Some(preview) = self.incsearch.take() else {
            return;
        };
        self.cursor = preview.start;
        if keep {
            return;
        }
        match preview.previous {
            Some((pattern, direction)) => {
                let ignore_case = self.options.borrow().ignorecase;
                let _ = self
                    .search
                    .run(&self.buffer, &pattern, direction, ignore_case);
            }
            None => self.search.clear(),
        }
    }

    /// Start asking about each match of `spec`, one at a time.
    ///
    /// Everything the answers do belongs to one history group: the one the
    /// command that started this is running in, which `run_command` leaves open
    /// while questions are up. `finish_confirm` closes it, so a `:s///c` is one
    /// `u` however many matches were answered — the same as a `:s` that never
    /// asked.
    pub fn start_confirm(&mut self, spec: &Substitute) -> Result<(), crate::search::PatternError> {
        let pattern = regex::RegexBuilder::new(&spec.pattern)
            .case_insensitive(spec.ignore_case)
            .build()
            .map_err(crate::search::PatternError::from)?;
        let rows = self.substitute_rows(spec);
        self.confirm = Some(Confirming {
            spec: spec.clone(),
            pattern,
            rows,
            row_at: 0,
            from: 0,
            count: 0,
            changed: Vec::new(),
            asked: false,
            asking: None,
        });
        self.advance_confirm();
        Ok(())
    }

    /// Look for the next match to ask about, and ask about it.
    fn advance_confirm(&mut self) {
        loop {
            let Some(state) = self.confirm.as_mut() else {
                return;
            };
            let Some(row) = state.rows.get(state.row_at).copied() else {
                self.finish_confirm();
                return;
            };
            let line = self.buffer.line_string(row);
            let chars: Vec<char> = line.chars().collect();
            let rest: String = chars.iter().skip(state.from).collect();
            match state.pattern.find(&rest) {
                Some(found) => {
                    let start = state.from + rest[..found.start()].chars().count();
                    let end = start + found.as_str().chars().count();
                    let replacement = state
                        .pattern
                        .replace(found.as_str(), state.spec.replacement.as_str())
                        .into_owned();
                    state.asking = Some((row, start, end, replacement));
                    state.asked = true;
                    let prompt = state.prompt();
                    self.message = prompt;
                    return;
                }
                None => {
                    state.row_at += 1;
                    state.from = 0;
                }
            }
        }
    }

    /// Answer the question that is up: `y`, `n`, `a`, `q`, `l`, or Escape.
    pub fn answer_confirm(&mut self, answer: &str) {
        let Some((row, start, end, replacement)) =
            self.confirm.as_ref().and_then(|state| state.asking.clone())
        else {
            return;
        };
        match answer {
            "y" | "l" | "a" => {
                let inserted: Vec<char> = replacement.chars().collect();
                let line_start = self.buffer.line_col_to_char(row, 0);
                let at = line_start + start;
                self.replace_range(
                    at,
                    line_start + end,
                    &replacement,
                    Some(at + inserted.len()),
                );
                if let Some(state) = self.confirm.as_mut() {
                    state.count += 1;
                    if !state.changed.contains(&row) {
                        state.changed.push(row);
                    }
                    state.from = start + inserted.len();
                    state.asking = None;
                }
                match answer {
                    "a" => self.finish_confirm_all(),
                    "l" => self.finish_confirm(),
                    _ => self.advance_confirm(),
                }
            }
            "n" => {
                if let Some(state) = self.confirm.as_mut() {
                    state.from = end;
                    state.asking = None;
                }
                self.advance_confirm();
            }
            // `q` gives up on the rest and keeps what was done; so does
            // Escape. A key that is not an answer at all is not one.
            "q" | "<Esc>" => self.finish_confirm(),
            _ => {}
        }
    }

    /// Replace everything that is left, for `a`.
    fn finish_confirm_all(&mut self) {
        let mut guard = 0;
        loop {
            let asking = self.confirm.as_ref().and_then(|state| state.asking.clone());
            if asking.is_none() {
                // Nothing is being asked about, so look for the next one.
                self.advance_confirm();
                let nothing_asked = self
                    .confirm
                    .as_ref()
                    .map(|state| state.asking.is_none())
                    .unwrap_or(true);
                if nothing_asked {
                    return;
                }
            } else {
                self.answer_confirm("y");
            }
            guard += 1;
            if guard > MAX_CONFIRM_STEPS {
                self.finish_confirm();
                return;
            }
        }
    }

    /// The questions are over: close the group they shared and say what was done.
    fn finish_confirm(&mut self) {
        let Some(state) = self.confirm.take() else {
            return;
        };
        self.history.end_group(&mut self.buffer);
        self.message = if state.asked {
            format!(
                "{} substitution{} on {} line{}",
                state.count,
                if state.count == 1 { "" } else { "s" },
                state.changed.len(),
                if state.changed.len() == 1 { "" } else { "s" }
            )
        } else {
            format!("E486: Pattern not found: {}", state.spec.pattern)
        };
        self.redraw = true;
    }

    /// Open the completion list on the words in this buffer, move along it, or
    /// filter what is up.
    ///
    /// `step` is `Some` for `<C-n>` and `<C-p>`, which move the pick and *type*
    /// it — vim's completion types as it goes, and the list is a thing you look
    /// at while typing rather than a menu you confirm afterwards. `None` means a
    /// keystroke happened, and what is in the buffer now is what filters.
    ///
    /// The words are gathered once, when the list opens: a popup that rescanned
    /// the whole file on every keystroke would be a popup nobody could type
    /// through.
    pub fn complete(&mut self, step: Option<isize>) {
        let at = self.cursor.char_idx(&self.buffer);
        let (_, prefix) = completion::prefix_at(&self.buffer, at);
        // A session with a language server may have better words than the file
        // does. It is asked every time the list is opened or moved, and the
        // answer replaces this list when it comes.
        if step.is_some() {
            self.lsp_ask = Some(Ask::Completion {
                row: self.cursor.row,
                col: self.cursor.col,
            });
        }
        match (self.completion.as_ref().is_some(), step) {
            (false, None) => return,
            (false, Some(_)) => self.open_completion(),
            (true, None) => self.filter_completion(&prefix),
            (true, Some(step)) => self.step_completion(step),
        }
        self.redraw = true;
    }

    /// Gather the words, put the list up, and type the first of them.
    fn open_completion(&mut self) {
        let at = self.cursor.char_idx(&self.buffer);
        let (start, prefix) = completion::prefix_at(&self.buffer, at);
        let mut all = completion::words(&self.buffer, self.cursor.row, COMPLETION_LIMIT);
        // The word being typed is not a completion of itself — but an empty
        // prefix is not a word, and `<C-n>` on a space offers everything, which
        // is how a person sees what is in a file.
        if !prefix.is_empty() {
            all.retain(|item| item.label != prefix);
        }
        let shown = completion::matching(&all, &prefix);
        if shown.is_empty() {
            return;
        }
        self.completion = Some(Completion {
            start,
            typed: prefix.clone(),
            prefix,
            all,
            shown,
            selected: 0,
            inserted: false,
        });
        // Everything the list types belongs to one step of undo, the way an
        // insert session does.
        self.history.begin_group();
        // The first item, typed: the first one of these is the one that is
        // already picked, so the pick does not move.
        self.step_completion(0);
    }

    /// Move the pick by `step`, and type what is picked.
    fn step_completion(&mut self, step: isize) {
        let Some(state) = self.completion.as_ref() else {
            return;
        };
        let count = state.shown.len() as isize;
        if count == 0 {
            return;
        }
        let selected = (state.selected as isize + step).rem_euclid(count) as usize;
        let item = state.shown[selected].clone();
        let (typed, stops) = if item.snippet {
            completion::snippet(&item.insert)
        } else if item.insert.is_empty() {
            (item.label.clone(), Vec::new())
        } else {
            (item.insert.clone(), Vec::new())
        };
        // A snippet's stops are ranges in the buffer once it is typed, which is
        // where they are kept until the session ends or the last one is passed.
        self.snippet_stops = stops
            .iter()
            .map(|stop| {
                let (start, end) = stop.span;
                completion::Stop {
                    span: (state.start + start, state.start + end),
                    choices: stop.choices.clone(),
                    number: stop.number,
                    mirror: stop.mirror,
                }
            })
            .collect();
        self.snippet_choice = 0;
        // Nothing has been stepped to yet: the cursor has just typed the text.
        self.snippet_step = 0;
        let start = state.start;
        let at = self.cursor.char_idx(&self.buffer);
        // What is replaced is the word being completed and nothing else: the
        // text after the cursor belongs to the line, not to the completion.
        self.replace_range(start, at, &typed, Some(start + typed.chars().count()));
        if let Some(state) = self.completion.as_mut() {
            state.selected = selected;
            state.prefix = typed;
            state.inserted = true;
        }
        // The item the list just moved to is the one worth knowing more about:
        // a server is asked for its details lazily, one item at a time.
        self.completion_resolve = Some(item.label);
        self.after_edit();
    }

    /// A keystroke happened: filter by what is in front of the cursor, and put
    /// the list away when nothing is left to offer.
    fn filter_completion(&mut self, prefix: &str) {
        let Some(state) = self.completion.as_mut() else {
            return;
        };
        state.prefix = prefix.to_string();
        state.shown = completion::matching(&state.all, prefix);
        state.selected = state.selected.min(state.shown.len().saturating_sub(1));
        if state.shown.is_empty() {
            // Nothing matches what has been typed. The text stays: it is what
            // the person wrote, and being wrong about a word is not a reason to
            // take it away.
            self.close_completion();
        }
    }

    /// Put the list away, keeping whatever it has typed.
    fn close_completion(&mut self) {
        if self.completion.take().is_some() {
            self.history.end_group(&mut self.buffer);
            self.redraw = true;
        }
    }

    /// The question that is waiting, if one is: a session that has a language
    /// server takes it and asks.
    pub fn take_lsp_ask(&mut self) -> Option<Ask> {
        self.lsp_ask.take()
    }

    /// Keep what the list has typed, and put it away. This is `<C-y>`, and with
    /// vim's completion it is usually nothing more than "stop".
    pub fn accept_completion(&mut self) {
        self.close_completion();
    }

    /// Step to the next place a snippet wants something typed.
    ///
    /// The stops were recorded when the snippet was typed, as ranges in the
    /// buffer, and `snippet_step` counts how many have been stepped to — so a
    /// fresh snippet is at zero and `<Tab>` goes to the first. Past the last
    /// stop there is nothing to step to, the session is over, and the key means
    /// what it always meant.
    pub fn snippet_step(&mut self, forward: bool) -> bool {
        if self.snippet_stops.is_empty() {
            return false;
        }
        // A place written twice is stepped to once: the first writing is where
        // it is typed, and the others follow it.
        let wanted: Vec<usize> = self
            .snippet_stops
            .iter()
            .enumerate()
            .filter(|(_, stop)| !stop.mirror)
            .map(|(index, _)| index)
            .collect();
        let position = wanted
            .iter()
            .position(|index| *index + 1 == self.snippet_step);
        let step = if forward {
            match position {
                Some(at) if at + 1 < wanted.len() => wanted[at + 1],
                Some(_) => {
                    self.end_snippet();
                    return false;
                }
                // Nothing has been stepped to: the first place is the next one.
                None if self.snippet_step == 0 => wanted[0],
                None => return false,
            }
        } else {
            match position {
                Some(at) if at > 0 => wanted[at - 1],
                _ => return false,
            }
        };
        let next = step;
        self.snippet_step = next + 1;
        self.select_stop(next);
        self.after_edit();
        true
    }

    /// Type the next thing the stop the cursor is on allows.
    ///
    /// A stop written `${1|one,two|}` offers both; the one in the text is the
    /// current choice, and picking another replaces it in place. The stops after
    /// it move by however much the text grew or shrank, which is the whole
    /// complication: they are ranges, and a range is only true until the text in
    /// front of it changes.
    pub fn snippet_choose(&mut self, forward: bool) -> bool {
        if self.snippet_stops.is_empty() {
            return false;
        }
        // The stop the cursor is in: which one that is comes from where it is
        // rather than from how many have been stepped to, so that a cursor moved
        // into a snippet by hand picks from the place it is looking at.
        let at = self.cursor.char_idx(&self.buffer);
        let Some(current) = self
            .snippet_stops
            .iter()
            .rposition(|stop| stop.span.0 <= at)
        else {
            return false;
        };
        let Some(stop) = self.snippet_stops.get(current).cloned() else {
            return false;
        };
        if stop.choices.len() < 2 {
            return false;
        }
        let count = stop.choices.len();
        let choice = if forward {
            (self.snippet_choice + 1) % count
        } else {
            (self.snippet_choice + count - 1) % count
        };
        let text = stop.choices[choice].clone();
        let (start, end) = stop.span;
        if start > self.buffer.len_chars() || end > self.buffer.len_chars() {
            // The text is not where it was: the session is over rather than
            // rewriting something the person is working on.
            self.end_snippet();
            return false;
        }
        // The replace goes through the same path every edit does, which is what
        // keeps the stop's own range and the ones after it true — the delta
        // arithmetic lives in one place, and this is not it.
        self.replace_range(start, end, &text, Some(start + text.chars().count()));
        // What was picked stays selected, so that typing over it picks again.
        self.select_stop(current);
        self.snippet_choice = choice;
        self.after_edit();
        true
    }

    /// Make every other writing of a stop say what the stop says.
    ///
    /// Called for every edit, and does nothing unless one is being typed into a
    /// stop that has mirrors: the text of the stop as it now is goes into each
    /// of them, and the ranges after each of them move by the difference. The
    /// mirrors' own edits come back through here, which is what the flag is for.
    fn sync_snippet_mirrors(&mut self, start: usize, end: usize, written: usize) {
        if self.snippet_mirrors_busy || self.snippet_stops.is_empty() {
            return;
        }
        let Some(current) = self.snippet_stops.iter().position(|stop| {
            !stop.mirror && stop.span.0 <= start && start <= stop.span.1.max(stop.span.0)
        }) else {
            return;
        };
        // The stop being typed into is as long as the text in it *now*: the edit
        // that just happened is the newest part of it, and the range is stale
        // until this says otherwise.
        let delta = written as isize - end.saturating_sub(start) as isize;
        if delta != 0 {
            let stop = &mut self.snippet_stops[current];
            stop.span.1 = (stop.span.1 as isize + delta).max(stop.span.0 as isize) as usize;
            // And every range after it moves with it.
            let shift = |value: usize| (value as isize + delta).max(0) as usize;
            let (heading, rest) = self.snippet_stops.split_at_mut(current + 1);
            let _ = heading;
            for stop in rest.iter_mut() {
                stop.span = (shift(stop.span.0), shift(stop.span.1));
            }
        }
        let number = self.snippet_stops[current].number;
        let text = self.buffer.slice_text(
            self.snippet_stops[current].span.0,
            self.snippet_stops[current].span.1,
        );
        let mirrors: Vec<usize> = self
            .snippet_stops
            .iter()
            .enumerate()
            .filter(|(index, stop)| *index != current && stop.mirror && stop.number == number)
            .map(|(index, _)| index)
            .collect();
        if mirrors.is_empty() {
            return;
        }
        let old_len = self.snippet_stops[current]
            .span
            .1
            .saturating_sub(self.snippet_stops[current].span.0);
        let delta = text.chars().count() as isize - old_len as isize;
        self.snippet_mirrors_busy = true;
        for index in mirrors {
            let (from, to) = self.snippet_stops[index].span;
            if from > self.buffer.len_chars() || to > self.buffer.len_chars() {
                continue;
            }
            self.replace_range(from, to, &text, None);
            self.snippet_stops[index].span = (from, from + text.chars().count());
            if delta != 0 {
                let shift = |value: usize| (value as isize + delta).max(0) as usize;
                for stop in self.snippet_stops.iter_mut().skip(index + 1) {
                    stop.span = (shift(stop.span.0), shift(stop.span.1));
                }
            }
        }
        self.snippet_mirrors_busy = false;
    }

    /// Forget a snippet session: leaving insert mode ends it.
    pub fn end_snippet(&mut self) {
        self.snippet_stops.clear();
        self.snippet_step = 0;
        self.snippet_choice = 0;
        // A selection that was only there for the snippet goes with it.
        if self.mode == Mode::Insert {
            self.selection = None;
        }
    }

    /// Put the list away and put back what was typed before it opened. This is
    /// `<C-e>`, the difference between "no thanks" and "not that one".
    pub fn cancel_completion(&mut self) {
        let Some(state) = self.completion.as_ref() else {
            return;
        };
        let (start, typed) = (state.start, state.typed.clone());
        let at = self.cursor.char_idx(&self.buffer);
        if at > start {
            self.replace_range(start, at, &typed, Some(start + typed.chars().count()));
            self.after_edit();
        }
        self.close_completion();
    }

    /// Offer these items instead of what the buffer has. A language server's
    /// answer, arriving late, replaces whatever the popup is showing.
    pub fn set_completions(&mut self, items: Vec<CompletionItem>) {
        // Put back what was typed before this list started writing. A server is
        // answering about the word as it was asked about, and a local pick that
        // has already been typed into the buffer would filter the answer away —
        // the answer is better than the guess, so the guess goes back.
        if let Some(state) = self.completion.as_ref() {
            let (start, typed) = (state.start, state.typed.clone());
            let at = self.cursor.char_idx(&self.buffer);
            if at > start {
                self.replace_range(start, at, &typed, Some(start + typed.chars().count()));
                self.after_edit();
            }
        }
        let at = self.cursor.char_idx(&self.buffer);
        let (start, prefix) = completion::prefix_at(&self.buffer, at);
        let shown = completion::matching(&items, &prefix);
        if shown.is_empty() {
            return;
        }
        self.completion = Some(Completion {
            start,
            typed: prefix.clone(),
            prefix,
            all: items,
            shown,
            selected: 0,
            inserted: false,
        });
        self.history.begin_group();
        self.step_completion(1);
        self.redraw = true;
    }

    /// Set an option from what a person typed, which is what `:set` does.
    ///
    /// The value is read the way it reads in a script — `true`, `false`, a number
    /// — so the `:` line and `init.lua` cannot disagree about what a value means.
    pub fn set_option(&mut self, setting: Setting, value: &str) -> Result<(), String> {
        let param = match value {
            "true" => Param::Bool(true),
            "false" => Param::Bool(false),
            number => match number.parse::<i64>() {
                Ok(n) => Param::Int(n),
                Err(_) => Param::Str(value.to_string()),
            },
        };
        self.options.borrow_mut().set(setting, param)
    }

    /// What an option is set to, for `:set {name}?` and for the message that
    /// follows a set.
    pub fn option_text(&self, setting: Setting) -> String {
        match self.options.borrow().get(setting) {
            Param::Bool(value) => value.to_string(),
            Param::Int(value) => value.to_string(),
            other => format!("{other:?}"),
        }
    }

    /// Hand over the places the last answer named, and start the walk at the
    /// first: a list of somewhere to go is no use if it cannot be walked.
    pub fn set_places(&mut self, places: Vec<(PathBuf, usize, usize)>) {
        self.places = places;
        self.place_index = if self.places.is_empty() { 0 } else { 1 };
        self.remember_list(List::Places);
    }

    /// Note what is being shown, so that `:colder` can come back to it.
    ///
    /// The history holds the *lists*, not the windows: what a person wants to
    /// return to is the places a question named, and re-opening one is how they
    /// see it again. Ten of them is more than anybody walks back through.
    fn remember_list(&mut self, kind: List) {
        self.list_history.truncate(self.list_index);
        self.list_history.push(ListSnapshot {
            kind,
            places: self.places.clone(),
            targets: self.list_targets.clone(),
        });
        if self.list_history.len() > 10 {
            self.list_history.remove(0);
        }
        self.list_index = self.list_history.len();
    }

    /// Show the list `step` older (`-1`) or newer (`+1`), if there is one.
    pub fn walk_list_history(&mut self, step: isize) -> bool {
        if self.list_history.is_empty() {
            self.message = "E776: No location list".to_string();
            return false;
        }
        let wanted = self.list_index as isize + step;
        if wanted < 1 || wanted > self.list_history.len() as isize {
            self.message = "E553: No more items".to_string();
            return false;
        }
        self.list_index = wanted as usize;
        let snapshot = self.list_history[self.list_index - 1].clone();
        self.places = snapshot.places.clone();
        self.place_index = if self.places.is_empty() { 0 } else { 1 };
        // The window that was showing the other one goes away first, so that
        // walking back and forth does not pile windows up.
        if self.list.is_some() {
            let window = self.current_window;
            self.close_window(window);
        }
        match snapshot.kind {
            List::Places => self.open_places_list(),
            List::Diagnostics => self.open_diagnostics_list(),
            List::Settings => self.open_settings_window(),
            List::Commands => self.open_history_window(false),
            List::Searches => self.open_history_window(true),
        }
        self.list_index = wanted as usize;
        self.message = format!("list {} of {}", self.list_index, self.list_history.len());
        true
    }

    /// Step through the places the last answer named, and go to the one that is
    /// reached. Past either end the walk stops rather than wrapping: a list has
    /// ends, and a person who walks off one wants to know.
    pub fn walk_places(&mut self, forward: bool, count: usize) -> bool {
        if self.places.is_empty() {
            self.message = "E776: No location list".to_string();
            return false;
        }
        let last = self.places.len();
        let mut index = self.place_index.max(1);
        for _ in 0..count {
            if forward {
                if index >= last {
                    self.message = "E553: No more items".to_string();
                    return false;
                }
                index += 1;
            } else {
                if index <= 1 {
                    self.message = "E553: No more items".to_string();
                    return false;
                }
                index -= 1;
            }
        }
        self.place_index = index;
        let (path, row, col) = self.places[index - 1].clone();
        let _ = self.goto_location(&path, row, col);
        self.message = format!("{index} of {last}: {}", self.places[index - 1].0.display());
        true
    }

    /// Come back to the selection the last visual mode had.
    pub fn reselect(&mut self) -> bool {
        let Some((selection, cursor)) = self.last_visual else {
            return false;
        };
        self.selection = Some(selection);
        self.cursor = cursor;
        self.mode = Mode::Visual;
        self.clamp_normal_cursor();
        true
    }

    /// Go back to where insert mode was last left, and insert there.
    pub fn insert_again(&mut self) -> bool {
        let Some(cursor) = self.insert_again_at else {
            return false;
        };
        self.cursor = cursor;
        self.mode = Mode::Insert;
        self.clamp_normal_cursor();
        true
    }

    /// Open the list of places in a window of its own, the way a quickfix list
    /// is shown.
    ///
    /// The window holds a document whose lines *are* the places, in order, so the
    /// line the cursor is on names the place to go to — which is what `<CR>`
    /// reads. It is a document like any other, which is why nothing new was
    /// needed to draw it; what is new is that it is not for editing.
    pub fn open_places_list(&mut self) {
        self.open_places_list_where(None)
    }

    /// The same, with only the lines a pattern matches.
    ///
    /// A list with two hundred places in it is a list nobody reads; the filter is
    /// what makes one usable. The lines that survive keep their *places* — the
    /// targets are filtered with them, since a list and where its lines go are one
    /// thing.
    pub fn open_places_list_where(&mut self, filter: Option<&str>) {
        if self.places.is_empty() {
            self.message = "E776: No location list".to_string();
            return;
        }
        let Some(keep) = self.filter_rows(filter) else {
            return;
        };
        let mut text = String::new();
        let mut targets = Vec::new();
        let mut shown = 0;
        for (index, (path, row, col)) in self.places.iter().enumerate() {
            let line = format!(
                "{}:{}:{}  {}",
                path.display(),
                row + 1,
                col + 1,
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
            if !keep(index, &line) {
                continue;
            }
            shown += 1;
            text.push_str(&format!("{shown} {line}\n"));
            targets.push((path.clone(), *row, *col));
        }
        let count = targets.len();
        if count == 0 {
            self.message = format!("Pattern not found: {}", filter.unwrap_or("").trim());
            return;
        }
        let mut document = Document::new(Buffer::from_text(&text), Language::Plain);
        document.list = Some(List::Places);
        document.list_targets = targets;
        self.documents.push(document);
        let index = self.documents.len() - 1;
        self.split_window(Axis::Rows, Some(index));
        self.message = format!("{count} place{}", if count == 1 { "" } else { "s" });
    }

    /// Show what has been typed on the `:` line (or searched for) in a window of
    /// its own — vim's `q:` and `q/`.
    ///
    /// The lines are the history as it is, oldest first, and `<CR>` runs the one
    /// the cursor is on: a command history is a list of things to *do again*,
    /// which is what makes it a list rather than a message.
    pub fn open_history_window(&mut self, searches: bool) {
        let history = if searches {
            &self.search_history
        } else {
            &self.command_history
        };
        if history.is_empty() {
            self.message = if searches {
                "E776: No search history".to_string()
            } else {
                "E776: No command history".to_string()
            };
            return;
        }
        let count = history.len();
        let text: String = history.iter().map(|line| format!("{line}\n")).collect();
        let mut document = Document::new(Buffer::from_text(&text), Language::Plain);
        document.list = Some(if searches {
            List::Searches
        } else {
            List::Commands
        });
        // No places to walk to: what `<CR>` does here is *run* the line, which is
        // not a place in a file.
        document.list_targets = Vec::new();
        self.documents.push(document);
        let index = self.documents.len() - 1;
        self.split_window(Axis::Rows, Some(index));
        self.message = format!("{count} command{}", if count == 1 { "" } else { "s" });
    }

    /// Show every setting and its value, in a window of its own.
    ///
    /// A list rather than one long message: there are more settings than fit on a
    /// line, and a person reading them wants to scroll. It is read-only for the
    /// same reason every other list is — a list that can be edited is a list that
    /// can lie about what it says. `:set {name}={value}` is how one is changed.
    pub fn open_settings_window(&mut self) {
        let mut text = String::new();
        for setting in crate::settings::Setting::ALL.iter().copied() {
            text.push_str(&format!(
                "{}\n",
                format_args!("{:<12} {}", setting.name(), self.option_text(setting))
            ));
        }
        let count = crate::settings::Setting::ALL.len();
        let mut document = Document::new(Buffer::from_text(&text), Language::Plain);
        document.list = Some(List::Settings);
        // No places to walk to: what `<CR>` would go to is a setting's name, and
        // there is nowhere to go with it.
        document.list_targets = Vec::new();
        self.documents.push(document);
        let index = self.documents.len() - 1;
        self.split_window(Axis::Rows, Some(index));
        self.message = format!("{count} settings");
    }

    /// Which lines a filter keeps, or everything when there is none.
    ///
    /// The pattern is the same dialect a search uses, and so is the case rule; a
    /// pattern that does not compile is reported rather than silently keeping
    /// everything, and one that matches nothing says so instead of opening an
    /// empty window.
    fn filter_rows(&mut self, filter: Option<&str>) -> Option<RowFilter> {
        let Some(pattern) = filter.map(str::trim).filter(|p| !p.is_empty()) else {
            return Some(Box::new(|_, _| true));
        };
        // The match is remembered so that a pattern that finds nothing can say
        // so: the alternative is a window with no lines in it, which looks like a
        // bug rather than an answer.
        let matched = std::rc::Rc::new(std::cell::Cell::new(false));
        let ignore_case = self.options.borrow().ignorecase;
        let compiled = match regex::RegexBuilder::new(pattern)
            .case_insensitive(ignore_case)
            .build()
        {
            Ok(compiled) => compiled,
            Err(error) => {
                self.message = format!("E54: {error}");
                return None;
            }
        };
        let seen = matched.clone();
        Some(Box::new(move |_, line| {
            let found = compiled.is_match(line);
            if found {
                seen.set(true);
            }
            found
        }))
    }

    /// Close the window that is showing the list, if that is what it shows.
    pub fn close_places_list(&mut self) {
        if self.list.is_none() {
            self.message = "E776: No location list".to_string();
            return;
        }
        let window = self.current_window;
        self.close_window(window);
    }

    /// Go to the place the cursor's line names, when the document on screen is a
    /// list of places.
    pub fn open_place_at_cursor(&mut self) {
        // A history window is a list of things to do again rather than places to
        // go to, so `<CR>` runs the line the cursor is on — vim's `q:`.
        if matches!(self.list, Some(List::Commands) | Some(List::Searches)) {
            let row = self.cursor.row;
            let line = self.buffer.line_string(row).trim().to_string();
            if line.is_empty() {
                return;
            }
            let searches = self.list == Some(List::Searches);
            let window = self.current_window;
            self.close_window(window);
            // The line is run as if it had been typed at the prompt it came from:
            // a command for `q:`, a search for `q/`.
            if searches {
                self.run_search(&line, crate::search::Direction::Forward);
            } else {
                self.execute_cmdline(&line);
            }
            return;
        }
        if self.list.is_none() {
            // Anywhere else, `<CR>` is the motion it always was.
            self.run_command("move_down", &[]);
            return;
        }
        let row = self.cursor.row;
        let Some((path, line, col)) = self.list_targets.get(row).cloned() else {
            self.message = "E776: No location list".to_string();
            return;
        };
        // The window that shows the list goes away, the way a quickfix window
        // does, so that the place arrived at is not half a screen.
        let window = self.current_window;
        self.close_window(window);
        if path.as_os_str().is_empty() {
            // A list of what a server said about a file that has no name — an
            // unsaved buffer — has nowhere to go but here.
            self.cursor.row = line.min(self.buffer.line_count().saturating_sub(1));
            self.cursor.col = col;
            self.clamp_normal_cursor();
            return;
        }
        let _ = self.goto_location(&path, line, col);
    }

    /// Open what a language server said about this file in a window of its own.
    ///
    /// The same shape the list of places has — a document whose lines are the
    /// list — because a diagnostic is a line of text with a place attached, which
    /// is what a list line is.
    pub fn open_diagnostics_list(&mut self) {
        self.open_diagnostics_list_where(None)
    }

    /// The same, with only the lines a pattern matches — `:lopen warning` is the
    /// list of warnings.
    pub fn open_diagnostics_list_where(&mut self, filter: Option<&str>) {
        // Every document that is open, not only the one on screen: a server says
        // what it thinks about each file it has been told about, and a person
        // wants to know where the work is.
        let mut rows: Vec<(PathBuf, usize, usize, String)> = Vec::new();
        let current = self.current_document();
        for (index, document) in self.documents.iter().enumerate() {
            let diagnostics = if index == current {
                &self.diagnostics
            } else {
                &document.diagnostics
            };
            let path = document.buffer.path().map(Path::to_path_buf);
            for diagnostic in diagnostics {
                let name = path
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "[No Name]".to_string());
                rows.push((
                    path.clone().unwrap_or_default(),
                    diagnostic.start.0,
                    diagnostic.start.1,
                    format!(
                        "{name}:{}:{} {}",
                        diagnostic.start.0 + 1,
                        diagnostic.start.1 + 1,
                        diagnostic.line()
                    ),
                ));
            }
        }
        if rows.is_empty() {
            self.message = "No diagnostics".to_string();
            return;
        }
        if let Some(pattern) = filter.map(str::trim).filter(|p| !p.is_empty()) {
            let ignore_case = self.options.borrow().ignorecase;
            match regex::RegexBuilder::new(pattern)
                .case_insensitive(ignore_case)
                .build()
            {
                Ok(compiled) => rows.retain(|row| compiled.is_match(&row.3)),
                Err(error) => {
                    self.message = format!("E54: {error}");
                    return;
                }
            }
            if rows.is_empty() {
                self.message = format!("Pattern not found: {pattern}");
                return;
            }
        }
        let count = rows.len();
        let files = {
            let mut paths: Vec<&PathBuf> = rows.iter().map(|row| &row.0).collect();
            paths.sort();
            paths.dedup();
            paths.len()
        };
        let mut text = String::new();
        for (index, (_, _, _, line)) in rows.iter().enumerate() {
            text.push_str(&format!("{} {line}\n", index + 1));
        }
        let mut document = Document::new(Buffer::from_text(&text), Language::Plain);
        document.list = Some(List::Diagnostics);
        // A diagnostic is about a place in a file, which is what `<CR>` goes to.
        document.list_targets = rows
            .into_iter()
            .map(|(path, row, col, _)| (path, row, col))
            .collect();
        self.documents.push(document);
        let index = self.documents.len() - 1;
        self.split_window(Axis::Rows, Some(index));
        self.message = format!(
            "{count} diagnostic{} in {files} file{}",
            if count == 1 { "" } else { "s" },
            if files == 1 { "" } else { "s" }
        );
    }

    /// The places the last answer named, as one line, for `:clist`.
    pub fn places_text(&self) -> String {
        if self.places.is_empty() {
            return "No location list".to_string();
        }
        let mut parts = vec![format!("{} places", self.places.len())];
        for (index, (path, row, _)) in self.places.iter().enumerate() {
            let here = if index + 1 == self.place_index {
                ">"
            } else {
                " "
            };
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            parts.push(format!("{here}{} {name}:{}", index + 1, row + 1));
        }
        parts.join("   ")
    }

    /// The item a language server is being asked about, taken by the session.
    ///
    /// A completion answer gives a label and little else; the details — what the
    /// item is, what it does — come from a second question about the one item a
    /// person has actually selected.
    pub fn take_completion_resolve(&mut self) -> Option<String> {
        self.completion_resolve.take()
    }

    /// Fill in what a server said about one item.
    ///
    /// The list holds two copies of an item — every one that arrived, and the
    /// ones still matching what has been typed — and both are the same item, so
    /// both are told.
    pub fn set_completion_detail(&mut self, label: &str, detail: &str) {
        let Some(state) = self.completion.as_mut() else {
            return;
        };
        let mut found = false;
        for item in state.all.iter_mut().chain(state.shown.iter_mut()) {
            if item.label == label && item.detail != detail {
                item.detail = detail.to_string();
                found = true;
            }
        }
        if found {
            self.redraw = true;
        }
    }

    /// Ask what the thing under the cursor is, and where it is defined: a
    /// question the kernel cannot answer by itself.
    pub fn ask_server(&mut self, ask: Ask) {
        self.lsp_ask = Some(ask);
        self.message = "asking the language server...".to_string();
        self.redraw = true;
    }

    /// Go to a place in a file: open it if it is not open, and put the cursor
    /// on the position.
    ///
    /// A position from a server is a row and a column in characters by the time
    /// it gets here — the conversion happens where the protocol does.
    pub fn goto_location(
        &mut self,
        path: &Path,
        row: usize,
        col: usize,
    ) -> Result<(), BufferError> {
        if self.buffer.path().map(|open| open != path).unwrap_or(true) {
            let open = self.documents.iter().position(|document| {
                document
                    .buffer
                    .path()
                    .map(|open| open == path)
                    .unwrap_or(false)
            });
            match open {
                Some(index) => {
                    self.show_document(index);
                }
                None => self.open_file(path)?,
            }
        }
        let row = row.min(self.buffer.line_count().saturating_sub(1));
        let col = col.min(self.buffer.line_len(row));
        let at = self.buffer.line_col_to_char(row, col);
        self.cursor.set_char_idx(&self.buffer, at);
        self.clamp_cursor();
        self.after_edit();
        Ok(())
    }

    /// Replace what a language server has to say about this document.
    ///
    /// The kernel does not care where they came from: a client translates the
    /// protocol's positions into rows and character columns and hands them over.
    /// They are sorted, because that is what `]d` and `[d` walk.
    pub fn set_diagnostics(&mut self, diagnostics: Vec<Diagnostic>) {
        self.diagnostics = diagnostics;
        self.diagnostics.sort_by_key(diagnostic_order);
        self.redraw = true;
    }

    /// The diagnostic under the cursor, if one covers its line.
    pub fn diagnostic_here(&self) -> Option<&Diagnostic> {
        let row = self.cursor.row;
        let col = self.cursor.col;
        self.diagnostics
            .iter()
            .find(|diagnostic| match diagnostic.on_row(row) {
                Some((from, to)) => col >= from && col < to.max(from + 1),
                None => false,
            })
            .or_else(|| {
                // Nothing under the cursor: the first one on its line, so that
                // being on the line is enough to be told about it.
                self.diagnostics
                    .iter()
                    .find(|diagnostic| diagnostic.on_row(row).is_some())
            })
    }

    /// Go to the next or previous diagnostic, wrapping around, and say what it
    /// is when we get there.
    pub fn step_diagnostic(&mut self, forward: bool) -> bool {
        if self.diagnostics.is_empty() {
            self.message = "No diagnostics".to_string();
            return false;
        }
        let here = self.cursor.char_idx(&self.buffer);
        let row = self.cursor.row;
        let found = if forward {
            self.diagnostics
                .iter()
                .find(|diagnostic| diagnostic.start.0 > row)
                .or_else(|| self.diagnostics.first())
        } else {
            self.diagnostics
                .iter()
                .rev()
                .find(|diagnostic| diagnostic.start.0 < row)
                .or_else(|| self.diagnostics.last())
        };
        let Some(diagnostic) = found.cloned() else {
            return false;
        };
        let _ = here;
        let at = self
            .buffer
            .line_col_to_char(diagnostic.start.0.min(self.buffer.line_count() - 1), 0);
        self.cursor.set_char_idx(&self.buffer, at);
        self.clamp_cursor();
        self.message = format!("{}: {}", diagnostic.severity.name(), diagnostic.message);
        self.after_edit();
        true
    }

    /// The lines a substitute looks at: the range it named, or the one the
    /// cursor is on when it named none.
    ///
    /// Addresses are resolved against the buffer *now* — a line number in a `:`
    /// line means the line of the file as it is, which is how `:5s/a/b/` can be
    /// typed again after an edit moved that line.
    fn substitute_rows(&self, spec: &Substitute) -> Vec<usize> {
        let last = self.buffer.line_count().saturating_sub(1);
        let resolve = |address: Address| self.resolve_address(address).min(last);
        match (spec.from, spec.to) {
            (None, _) => vec![self.cursor.row],
            (Some(from), Some(to)) => {
                let (from, to) = (resolve(from), resolve(to));
                let (from, to) = (from.min(to), from.max(to));
                (from..=to).collect()
            }
            // One address is one line: `:5s/a/b/` is line five in vim, and
            // `:.s` is the cursor's line rather than the rest of the file.
            (Some(from), None) => vec![resolve(from)],
        }
    }

    /// Replace what the pattern in `spec` finds.
    ///
    /// Returns how many places were replaced and how many lines that was — the
    /// two numbers a person wants back — or the reason the pattern did not
    /// compile.
    ///
    /// Every line that changes is put back in *one* replacement, spanning the
    /// first and the last of them: a substitute is one change, so one `u` undoes
    /// one, however many lines it touched. The lines between them ride along
    /// unchanged, which is the price of a single step and a small one.
    pub fn substitute(
        &mut self,
        spec: &Substitute,
    ) -> Result<(usize, usize), crate::search::PatternError> {
        let pattern = regex::RegexBuilder::new(&spec.pattern)
            .case_insensitive(spec.ignore_case)
            .build()
            .map_err(crate::search::PatternError::from)?;

        let rows = self.substitute_rows(spec);

        // Which lines change, and what they become.
        let mut replacements: Vec<(usize, String)> = Vec::new();
        let mut count = 0;
        for row in rows {
            let line = self.buffer.line_string(row);
            let found = pattern.find_iter(&line).count();
            if found == 0 {
                continue;
            }
            let replaced = if spec.all {
                pattern.replace_all(&line, spec.replacement.as_str())
            } else {
                pattern.replacen(&line, 1, spec.replacement.as_str())
            };
            if replaced == line.as_str() {
                continue;
            }
            count += if spec.all { found } else { 1 };
            replacements.push((row, replaced.into_owned()));
        }
        let Some((first, _)) = replacements.first() else {
            return Ok((0, 0));
        };
        let first = *first;
        let last = replacements.last().expect("not empty").0;

        let start = self.buffer.line_col_to_char(first, 0);
        let end = self.buffer.line_col_to_char(last, 0)
            + self.buffer.line_len(last)
            + self.buffer.line_terminator_len(last);
        let mut text = String::new();
        let mut cursor = start;
        let mut row = first;
        for (changed, replacement) in &replacements {
            // The lines that did not change keep their own text, terminator and
            // all: they are inside the range, not part of the change.
            while row < *changed {
                text.push_str(&self.buffer.line_string(row));
                text.push_str(&self.buffer.line_terminator(row));
                row += 1;
            }
            cursor = start + text.chars().count();
            text.push_str(replacement);
            text.push_str(&self.buffer.line_terminator(row));
            row += 1;
        }

        // The cursor ends on the last line the substitute changed, the way vim
        // leaves it.
        self.replace_range(start, end, &text, Some(cursor));
        Ok((count, replacements.len()))
    }

    /// The line an address names, in the text as it is now.
    ///
    /// A mark that was never set has no line of its own, and a range that names
    /// one falls back to the cursor's line rather than 0 or the end.
    pub fn resolve_address(&self, address: Address) -> usize {
        let last = self.buffer.line_count().saturating_sub(1);
        match address {
            Address::Line(number) => number.saturating_sub(1).min(last),
            Address::Current => self.cursor.row.min(last),
            Address::Last => last,
            Address::Mark(letter) => self
                .marks
                .get(letter)
                .map_or(self.cursor.row, |mark| mark.row)
                .min(last),
        }
    }

    /// The lines an address range covers, in order, and nothing when the line
    /// named none.
    ///
    /// The second address defaults to the first, which is what `:5d` means, and
    /// to the last line when the first is the cursor's — `:.` is one line, and
    /// vim's `:.` is the same. A `:` with no range at all is `None`.
    pub fn resolve_range(&self, line: &str) -> Option<(usize, usize)> {
        let (from, to) = crate::substitute::address_range(line);
        let from = from?;
        let from = self.resolve_address(from);
        let to = to.map_or(from, |to| self.resolve_address(to));
        Some((from.min(to), from.max(to)))
    }

    /// Read a file in below the cursor's line, and say how many lines it had.
    ///
    /// The text is inserted after the *whole* line the cursor is on — its
    /// terminator included, which is added if it was the last line and had none
    /// — so that what is read comes in as lines of its own rather than joined to
    /// what was there.
    pub fn read_file_below(&mut self, path: &Path) -> Result<usize, BufferError> {
        // Unlike `:e`, a file that is not there is an error: `:read` is about
        // what is in a file, and there is nothing in this one.
        if !path.exists() {
            return Err(BufferError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                path.display().to_string(),
            )));
        }
        let text = Buffer::from_path(path)?.text();
        let lines = text.lines().count();
        let row = self.cursor.row;
        let mut at = self.buffer.line_col_to_char(row, 0) + self.buffer.line_len(row);
        let terminator = self.buffer.line_terminator_len(row);
        let mut insert = String::new();
        if terminator == 0 {
            insert.push('\n');
        }
        at += terminator;
        insert.push_str(&text);
        // A file that ends without a line break would leave the next line joined
        // to it, so one is added — which is what reading a file into a line means.
        if !insert.ends_with('\n') {
            insert.push('\n');
        }
        self.replace_range(at, at, &insert, Some(at));
        Ok(lines)
    }

    /// Fold the lines `from..=to`, and close the fold — which is what `zf` does.
    ///
    /// A fold of one line is not a fold: there is nothing to hide. Folds are kept
    /// sorted by where they start, which is what lets the drawing walk them in
    /// order.
    pub fn fold_lines(&mut self, from: usize, to: usize) -> bool {
        let last = self.buffer.line_count().saturating_sub(1);
        let (start, end) = (from.min(last), to.min(last));
        if end <= start {
            // A fold of one line hides nothing, so there is nothing to make. (Vim
            // beeps; this says why, without borrowing an error number it does not
            // mean.)
            self.message = "cannot fold one line".to_string();
            return false;
        }
        // Folds nest: a fold inside another is a fold of its own, which is what
        // vim means by a fold's *level* being its nesting. The one thing that is
        // replaced is a fold over exactly the same lines — `zf` twice is one fold.
        self.folds
            .retain(|fold| !(fold.start == start && fold.end == end));
        self.folds.push(Fold {
            start,
            end,
            closed: true,
        });
        self.folds.sort_by_key(|fold| fold.start);
        // The cursor goes to the fold's first line, which is the one line of it
        // that is still on screen.
        self.cursor.row = start;
        true
    }

    /// Move the cursor to the start of the next fold, or the `count`th one — `zj`.
    ///
    /// Every fold counts, open or closed: `zj` is "go to the next fold", and a fold
    /// that is open is still a fold. (The first attempt filtered on `closed`, on the
    /// reasoning that an open fold is not in the way — the test's trace showed what
    /// that costs: with the only fold before the cursor open, `zk` said there was no
    /// fold at all.)
    pub fn goto_next_fold(&mut self, count: usize) -> bool {
        let row = self.cursor.row;
        let mut target = None;
        for _ in 0..count.max(1) {
            let next = self
                .folds
                .iter()
                .filter(|fold| fold.start > target.unwrap_or(row))
                .map(|fold| fold.start)
                .min();
            match next {
                Some(start) => target = Some(start),
                None => break,
            }
        }
        match target {
            Some(row) => {
                self.cursor.row = row;
                self.cursor.col = 0;
                true
            }
            None => {
                self.message = "E490: No fold found".to_string();
                false
            }
        }
    }

    /// The same backwards — `zk`.
    pub fn goto_previous_fold(&mut self, count: usize) -> bool {
        let row = self.cursor.row;
        let mut target = None;
        for _ in 0..count.max(1) {
            let previous = self
                .folds
                .iter()
                .filter(|fold| fold.start < target.unwrap_or(row))
                .map(|fold| fold.start)
                .max();
            match previous {
                Some(start) => target = Some(start),
                None => break,
            }
        }
        match target {
            Some(row) => {
                self.cursor.row = row;
                self.cursor.col = 0;
                true
            }
            None => {
                self.message = "E490: No fold found".to_string();
                false
            }
        }
    }

    /// Whether a blockwise selection is being made, which is the one place the
    /// cursor may go past the end of a line.
    pub fn is_making_a_block(&self) -> bool {
        self.mode == Mode::Visual
            && self
                .selection
                .is_some_and(|selection| selection.kind == VisualKind::Blockwise)
    }

    /// The file a view of this document is kept in.
    ///
    /// Vim keeps views in `~/.vim/view` under a mangled name; this keeps them in the
    /// config directory under the file's own name and a hash of its full path, so
    /// that two files called `main.rs` in different projects do not share one.
    pub fn view_path(&self) -> Option<PathBuf> {
        let name = self.buffer.path()?;
        let dir = match &self.view_dir {
            Some(dir) => dir.clone(),
            None => directories::ProjectDirs::from("", "", "inkstone")?
                .config_dir()
                .join("views"),
        };
        // FNV-1a over the path's bytes: a small hash with no dependency, and enough
        // to keep two same-named files apart.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.to_string_lossy().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let stem = name
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".to_string());
        Some(dir.join(format!("{stem}-{hash:016x}")))
    }

    /// Write this window's view — its folds and where the cursor is — to the file
    /// `:loadview` reads.
    pub fn make_view(&mut self) -> std::io::Result<()> {
        let Some(path) = self.view_path() else {
            self.message = "E474: No file name, so nowhere to keep a view".to_string();
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut text = format!(
            "cursor {} {}\ntop {}\n",
            self.cursor.row, self.cursor.col, self.viewport.top_line
        );
        for fold in &self.folds {
            text.push_str(&format!(
                "fold {} {} {}\n",
                fold.start,
                fold.end,
                if fold.closed { "closed" } else { "open" }
            ));
        }
        std::fs::write(&path, text)?;
        self.message = format!("view written: {}", path.display());
        Ok(())
    }

    /// Read a view back: the folds and the position it had.
    pub fn load_view(&mut self) -> std::io::Result<()> {
        let Some(path) = self.view_path() else {
            self.message = "E474: No file name, so nowhere to look for a view".to_string();
            return Ok(());
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            self.message = format!("no view for {}", self.buffer.display_name());
            return Ok(());
        };
        let last = self.buffer.line_count().saturating_sub(1);
        self.folds.clear();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            match parts.next() {
                Some("fold") => {
                    let start = parts.next().and_then(|n| n.parse::<usize>().ok());
                    let end = parts.next().and_then(|n| n.parse::<usize>().ok());
                    let closed = parts.next() == Some("closed");
                    if let (Some(start), Some(end)) = (start, end) {
                        // A view made before the file changed can name lines that are
                        // not there any more: the ones that are not are dropped.
                        if start < end && end <= last {
                            self.folds.push(Fold { start, end, closed });
                        }
                    }
                }
                Some("cursor") => {
                    let row = parts.next().and_then(|n| n.parse::<usize>().ok());
                    let col = parts.next().and_then(|n| n.parse::<usize>().ok());
                    if let (Some(row), Some(col)) = (row, col) {
                        self.cursor.row = row.min(last);
                        self.cursor.col = col;
                    }
                }
                Some("top") => {
                    if let Some(top) = parts.next().and_then(|n| n.parse::<usize>().ok()) {
                        self.viewport.top_line = top.min(last);
                    }
                }
                _ => {}
            }
        }
        self.folds.sort_by_key(|fold| fold.start);
        self.settle_cursor();
        self.after_edit();
        self.message = format!("{} fold(s) restored", self.folds.len());
        Ok(())
    }

    /// Work the folds out from the indent of the lines — `'foldmethod'` is `indent`.
    ///
    /// Vim's rule, in its own words: the fold level of a line is its indent divided by
    /// `'shiftwidth'`, rounded down (this editor indents by `'tabstop'`, so that is
    /// what divides); a line indented further than the one before it starts a fold,
    /// which ends at the last line that is at least as far in; and a blank line gets
    /// the *lower* of the levels around it, so it belongs to the fold that encloses
    /// it rather than starting one of its own.
    ///
    /// The folds are worked out when the method is set and by `zx`, not after every
    /// keystroke: a file's indentation does not usually change shape while it is
    /// being edited, and the alternative is walking the whole buffer per character.
    /// (The *line numbers* of existing folds are kept up to date by every edit, so a
    /// fold made this way does not drift.)
    pub fn compute_indent_folds(&mut self) -> usize {
        let options = self.options.borrow().clone();
        let tabstop = options.tabstop.max(1);
        let ignore: Vec<char> = options.foldignore.chars().collect();
        let nestmax = options.foldnestmax.max(1);
        let lines = self.buffer.line_count();
        // A line's indent, or `None` when it is *ignored*: blank, or starting with a
        // character in `'foldignore'` once its own white space is skipped — which is
        // what `#` is for in C, where a preprocessor line is not indented like the
        // code inside it.
        let indent_of = |row: usize| -> Option<usize> {
            let text = self.buffer.line_string(row);
            let indent = text.chars().take_while(|c| c.is_whitespace()).count();
            match text.chars().nth(indent) {
                None => None,
                Some(first) if ignore.contains(&first) => None,
                Some(_) => Some(indent),
            }
        };
        let mut levels = Vec::with_capacity(lines);
        for row in 0..lines {
            match indent_of(row) {
                Some(indent) => levels.push(indent / tabstop),
                // A blank line takes the lower of the levels around it, which is what
                // keeps a blank line inside the fold it is written in.
                None => {
                    let above = (0..row).rev().find_map(&indent_of).map(|i| i / tabstop);
                    let below = (row + 1..lines).find_map(&indent_of).map(|i| i / tabstop);
                    levels.push(match (above, below) {
                        (Some(a), Some(b)) => a.min(b),
                        (Some(a), None) => a,
                        (None, Some(b)) => b,
                        (None, None) => 0,
                    });
                }
            }
        }
        self.folds.clear();
        for row in 0..lines {
            let level = levels[row];
            if level == 0 || (row > 0 && levels[row - 1] >= level) {
                continue;
            }
            // `'foldnestmax'`: a fold deeper than that is not made. The level of a
            // fold *is* its nesting, so the level is what is compared.
            if level > nestmax {
                continue;
            }
            // The fold runs to the last line that is at least as far in.
            let end = (row + 1..lines)
                .take_while(|r| levels[*r] >= level)
                .last()
                .unwrap_or(row);
            if end > row {
                self.folds.push(Fold {
                    start: row,
                    end,
                    // Not closed *by hand*: with `indent` folding what is shown is
                    // decided by `'foldlevel'`, which `zm` and `zr` move. (A fold a
                    // person made with `zf` is closed by hand, and stays closed
                    // whatever the level says.)
                    closed: false,
                });
            }
        }
        self.folds.sort_by_key(|fold| fold.start);
        self.settle_cursor();
        self.folds.len()
    }

    /// Whether the folds are worked out from the indent, which is what `zx` asks
    /// about and what `:set foldmethod=indent` turns on.
    pub fn foldmethod_is_indent(&self) -> bool {
        self.options.borrow().foldmethod == crate::settings::FoldMethod::Indent
    }

    /// The innermost fold the cursor's line is inside, if it is inside one.
    ///
    /// Without levels the innermost is the smallest range, and a fold whose first
    /// line is the cursor's counts: that line is the one that is drawn.
    pub fn fold_at_cursor(&self) -> Option<usize> {
        let row = self.cursor.row;
        self.folds
            .iter()
            .enumerate()
            .filter(|(_, fold)| row >= fold.start && row <= fold.end)
            .min_by_key(|(_, fold)| fold.end - fold.start)
            .map(|(index, _)| index)
    }

    /// Open or close the fold the cursor is in — `zo` and `zc`.
    pub fn set_fold_closed(&mut self, closed: bool) -> bool {
        let Some(index) = self.fold_at_cursor() else {
            self.message = "E490: No fold found".to_string();
            return false;
        };
        self.set_fold_closed_at(index, closed);
        true
    }

    /// Open or close one fold, by index.
    fn set_fold_closed_at(&mut self, index: usize, closed: bool) {
        self.folds[index].closed = closed;
        // A fold nested deeper than `'foldlevel'` stays shut whatever this says, so
        // opening one has to move the level out of its way — which is what makes `zo`
        // open the fold a person is looking at.
        if !closed && self.fold_depth(index) > self.options.borrow().foldlevel.0 {
            let depth = self.fold_depth(index);
            self.options.borrow_mut().foldlevel = crate::settings::Count(depth);
        }
        if closed {
            self.cursor.row = self.folds[index].start;
        }
        self.after_edit();
    }

    /// Open or close every fold — `zR` and `zM`.
    pub fn set_all_folds_closed(&mut self, closed: bool) -> bool {
        if self.folds.is_empty() {
            self.message = "E490: No fold found".to_string();
            return false;
        }
        for fold in &mut self.folds {
            fold.closed = closed;
        }
        // `zR` and `zM` are about *all* of them, so they move the level out of the
        // way as well: a fold nested deeper than `'foldlevel'` is drawn closed
        // whatever its own flag says, and `zR` has to open those too.
        self.options.borrow_mut().foldlevel =
            crate::settings::Count(if closed { 0 } else { self.folds.len() + 1 });
        if closed {
            self.settle_cursor();
        }
        self.after_edit();
        true
    }

    /// Take away the fold the cursor is in — `zd`.
    pub fn delete_fold_at_cursor(&mut self) -> bool {
        let Some(index) = self.fold_at_cursor() else {
            self.message = "E490: No fold found".to_string();
            return false;
        };
        self.folds.remove(index);
        true
    }

    /// Remember the folds as they are *before* an edit, at the place in the history
    /// that edit is about to take.
    ///
    /// `fold_states[i]` is the state *after* `i` steps, so the stack reads the way the
    /// history does: whatever `steps` the history lands on, the entry at that index is
    /// the folds that belong to it. Each edit writes two entries — the state it found
    /// and the state it leaves — and the truncate drops the ones an undone step can no
    /// longer reach.
    fn record_fold_state(&mut self) {
        let steps = self.history.steps();
        self.fold_states.truncate(steps);
        self.fold_states.push(self.folds.clone());
    }

    /// Put the folds back to what they were at the step the history has landed on.
    ///
    /// Every entry is written by the *edit* that follows it — `fold_states[i]` is the
    /// folds before step `i + 1`, which is also the state step `i` left — so an undo
    /// and a redo have nothing to write: they only have to read the entry for the step
    /// they land on. What they must *not* do is leave entries for steps that can no
    /// longer be reached, which is what the truncate is for.
    fn restore_folds(&mut self) {
        let steps = self.history.steps();
        // One step *ahead* is kept as well: a redo goes there, and the entry for it is
        // the state the step being undone left. An edit made after an undo drops it
        // again — its own truncate is `steps + 1` — which is what keeps an undone step
        // from being redone to a stale fold.
        self.fold_states.truncate(steps + 2);
        if let Some(folds) = self.fold_states.get(steps).cloned() {
            self.folds = folds;
        }
    }

    /// A `:s` line with its range adjusted to any closed fold it touches.
    ///
    /// The line is rebuilt with the range spelled out in front of it, which is where a
    /// substitute reads its range from — the one the dispatcher resolved is not passed
    /// on. A line whose range does not change is left exactly as it was written, so
    /// `:%s/…/` stays `%`.
    fn substitute_line_with_fold_range(&self, line: &str) -> String {
        let written = self.resolve_range(line);
        let adjusted = match written {
            Some((from, to)) => self.widen_lines_over_folds(from, to),
            None => {
                // No range of its own: a substitute runs over the cursor's line, so
                // that is what is adjusted — which is the case the manual's example is
                // about.
                let row = self.cursor.row;
                match (0..self.folds.len()).find(|index| {
                    let fold = self.folds[*index];
                    self.fold_is_shut(*index) && row >= fold.start && row <= fold.end
                }) {
                    Some(index) => (self.folds[index].start, self.folds[index].end),
                    None => return line.to_string(),
                }
            }
        };
        if written == Some(adjusted) {
            return line.to_string();
        }
        let rest = crate::substitute::strip_range(line).unwrap_or(line);
        let (from, to) = adjusted;
        format!("{},{}{rest}", from + 1, to + 1)
    }

    /// Widen a range of lines to cover every closed fold it touches.
    pub(crate) fn widen_lines_over_folds(&self, from: usize, to: usize) -> (usize, usize) {
        let (mut first, mut last) = (from.min(to), from.max(to));
        for (index, fold) in self.folds.iter().enumerate() {
            if !self.fold_is_shut(index) {
                continue;
            }
            if fold.start <= last && fold.end >= first {
                first = first.min(fold.start);
                last = last.max(fold.end);
            }
        }
        (first, last)
    }

    /// A span widened to cover every *closed* fold it touches.
    ///
    /// The manual's rule: "when using an operator, a closed fold is included as a
    /// whole. Thus `dl` deletes the whole closed fold under the cursor." The span's
    /// lines are what is compared, since a fold is a range of lines.
    pub(crate) fn widen_over_folds(&self, span: crate::command::Span) -> crate::command::Span {
        let mut first = self.buffer.char_to_line_col(span.start).0;
        let mut last = self.buffer.char_to_line_col(span.end.saturating_sub(1)).0;
        let mut linewise = span.linewise;
        for (index, fold) in self.folds.iter().enumerate() {
            if !self.fold_is_shut(index) {
                continue;
            }
            // Touching is enough: a range that starts or ends inside the fold takes
            // the whole of it.
            if fold.start <= last && fold.end >= first {
                first = first.min(fold.start);
                last = last.max(fold.end);
                linewise = true;
            }
        }
        if first == self.buffer.char_to_line_col(span.start).0
            && last == self.buffer.char_to_line_col(span.end.saturating_sub(1)).0
        {
            return span;
        }
        let lines = last - first + 1;
        let (start, end) = crate::command::line_span(self, first, lines);
        crate::command::Span {
            start,
            end,
            linewise,
        }
    }

    /// How deeply a fold is nested: one plus the number of folds that contain it.
    ///
    /// A fold's *level* is its nesting — the manual says so in as many words — and
    /// that is what `'foldlevel'` is compared against.
    pub fn fold_depth(&self, index: usize) -> usize {
        let Some(fold) = self.folds.get(index).copied() else {
            return 0;
        };
        let outer = self
            .folds
            .iter()
            .filter(|other| other.start < fold.start && other.end >= fold.end)
            .count();
        outer + 1
    }

    /// Whether a fold is *drawn* closed: either it was closed, or it is nested
    /// deeper than `'foldlevel'` — which is what `zm` and `zr` move.
    pub fn fold_is_shut(&self, index: usize) -> bool {
        fold_is_shut(&self.folds, self.options.borrow().foldlevel.0, index)
    }

    /// `zm` — show one level fewer; `zr` — one more.
    ///
    /// Vim's `'foldlevel'`: a fold nested deeper than it is drawn as one line. Zero
    /// means "as the folds are", and `zR` is as open as it goes.
    pub fn step_fold_level(&mut self, more: bool) -> bool {
        if self.folds.is_empty() {
            self.message = "E490: No fold found".to_string();
            return false;
        }
        let deepest = (0..self.folds.len())
            .map(|i| self.fold_depth(i))
            .max()
            .unwrap_or(0);
        let current = self.options.borrow().foldlevel.0;
        let next = if more {
            (current + 1).min(deepest)
        } else {
            current.saturating_sub(1)
        };
        self.options.borrow_mut().foldlevel = crate::settings::Count(next);
        self.settle_cursor();
        self.after_edit();
        self.message = format!("foldlevel={next}");
        true
    }

    /// Whether a line is hidden inside a closed fold.
    pub fn line_is_folded(&self, row: usize) -> bool {
        (0..self.folds.len()).any(|index| {
            self.fold_is_shut(index)
                && row > self.folds[index].start
                && row <= self.folds[index].end
        })
    }

    /// The fold that *starts* at `row` and is drawn closed, if one is: the line that
    /// stands in place of the range.
    ///
    /// The folds are kept sorted by where they start, so the first one found is the
    /// outermost — which is the one that decides how far the walk skips, since a
    /// closed fold takes everything inside it with it.
    pub fn closed_fold_at(&self, row: usize) -> Option<Fold> {
        (0..self.folds.len())
            .find(|index| self.fold_is_shut(*index) && self.folds[*index].start == row)
            .map(|index| self.folds[index])
    }

    /// Put the cursor on a line that is on screen: a closed fold hides the lines
    /// after its first one, and a cursor inside one has nowhere to be.
    pub fn settle_cursor(&mut self) {
        let came_from = self.last_cursor_row;
        self.settle_cursor_from(came_from);
    }

    /// The same, told which line the cursor came from.
    ///
    /// Which way it was going is what decides where it lands: a cursor moving *down*
    /// into a closed fold is on its way past it and comes out at the line after —
    /// which is what makes `j` step over a fold rather than into it — and one moving
    /// *up* lands on the fold's first line. An edit that did not move the cursor at
    /// all (closing a fold, say) puts it on that first line, which is the line that
    /// is drawn.
    fn settle_cursor_from(&mut self, came_from: usize) {
        let row = self.cursor.row;
        let Some(fold) = (0..self.folds.len())
            .filter(|index| {
                let fold = self.folds[*index];
                self.fold_is_shut(*index) && row > fold.start && row <= fold.end
            })
            .min_by_key(|index| {
                let fold = self.folds[*index];
                fold.end - fold.start
            })
            .map(|index| self.folds[index])
        else {
            return;
        };
        self.cursor.row = if row > came_from {
            (fold.end + 1).min(self.buffer.line_count().saturating_sub(1))
        } else {
            fold.start
        };
        // (The manual's "the cursor is always *displayed* in the first column" while it
        // is on folded lines is about the drawing and not the cursor's value: setting
        // the column here would undo the motion an operator is about to take. The
        // roadmap has it, with the renderer as the place it belongs.)
    }

    /// The line an edit starting at `start` is on, or `None` when there are no folds
    /// to keep up to date.
    fn fold_line_for(&self, start: usize) -> Option<usize> {
        if self.folds.is_empty() {
            return None;
        }
        Some(
            self.buffer
                .char_to_line_col(start.min(self.buffer.len_chars()))
                .0,
        )
    }

    /// Move the folds to follow an edit of `delta` lines starting at line `at`: the
    /// folds after it move by the difference, and one that *contains* it grows or
    /// shrinks by it — which is what keeps a fold on the text it was put on.
    fn shift_folds(&mut self, at: usize, delta: isize) {
        let last = self.buffer.line_count().saturating_sub(1);
        for fold in &mut self.folds {
            if fold.start > at {
                // Entirely after the edit: the whole fold moves.
                fold.start = fold.start.saturating_add_signed(delta).min(last);
                fold.end = fold.end.saturating_add_signed(delta).min(last);
            } else if fold.end >= at {
                // The edit is inside it (or at its first line): the fold grows or
                // shrinks by the lines that came and went.
                fold.end = fold.end.saturating_add_signed(delta).min(last);
            }
            if fold.end < fold.start {
                fold.end = fold.start;
            }
        }
        // A fold of one line hides nothing, so the edit may have made one pointless.
        self.folds.retain(|fold| fold.end > fold.start);
    }

    /// The lines of the window that are drawn, from `top_line` on, for as many rows
    /// as the caller has room for.
    ///
    /// A closed fold is one line — its first — and the rest of it is not there. This
    /// is the whole of what folds do to drawing: the rows and the lines stop being
    /// the same thing.
    pub fn visible_lines(&self, top_line: usize, rows: usize) -> Vec<usize> {
        let last = self.buffer.line_count().saturating_sub(1);
        let mut out = Vec::with_capacity(rows);
        let mut row = top_line.min(last);
        while out.len() < rows && row <= last {
            out.push(row);
            // A closed fold takes its own range with it; anything else takes one
            // line.
            row = match self.closed_fold_at(row) {
                Some(fold) => fold.end + 1,
                None => row + 1,
            };
        }
        out
    }

    /// How many *visible* lines there are from `top_line` to `row`, which is what
    /// scrolling counts: a closed fold is one line and not the several it hides.
    pub fn visible_rows_between(&self, top_line: usize, row: usize) -> usize {
        let visible = self.visible_lines(top_line, self.buffer.line_count());
        visible.iter().take_while(|line| **line < row).count()
    }

    /// Join the lines `from..=to` into one, and say how many joins were made.
    ///
    /// Vim's `J`: each line is joined to the next by taking the terminator and the
    /// next line's leading whitespace out and putting a space in their place — so
    /// `one` and `   two` become `one two`. The whole range is one edit, which is
    /// what makes one `u` take it back.
    ///
    /// The separator is one space, or two after a full stop, a question mark or an
    /// exclamation mark when `'joinspaces'` is on — vim's rule, and the reason a
    /// joined pair of sentences keeps its double space.
    ///
    /// The clause about a bracket or a quote between the stop and the space (vim
    /// allows `.)` and `."`) is not done: the line's *last* character is what is
    /// looked at.
    pub fn join_lines(&mut self, from: usize, to: usize) -> usize {
        self.join_lines_with(from, to, false)
    }

    /// The same, with `tight` set: `gJ`, which inserts and removes no spaces at
    /// all — the terminators go and the next line follows exactly as it was, indent
    /// and all.
    pub fn join_lines_tight(&mut self, from: usize, to: usize) -> usize {
        self.join_lines_with(from, to, true)
    }

    fn join_lines_with(&mut self, from: usize, to: usize, tight: bool) -> usize {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        if to <= from {
            return 0;
        }
        // The range runs from the end of the first line to the end of the *last*
        // one's text: everything between them is a terminator or an indent, and the
        // lines themselves go back in the order they were in — each joined to the
        // one before it by a space, with its own indent gone. The last line's
        // terminator is outside the range, so it stays where it was.
        let start = self
            .buffer
            .line_col_to_char(from, self.buffer.line_len(from));
        let end = self.buffer.line_col_to_char(to, self.buffer.line_len(to));
        let joinspaces = self.options.borrow().joinspaces;
        let mut text = String::new();
        for row in from + 1..=to {
            // Two spaces after the end of a sentence, when the option says so. `gJ`
            // puts nothing in and takes nothing out.
            let ends_a_sentence = self
                .buffer
                .line_string(row - 1)
                .trim_end()
                .ends_with(['.', '?', '!']);
            if !tight {
                text.push_str(if joinspaces && ends_a_sentence {
                    "  "
                } else {
                    " "
                });
            }
            let next = self.buffer.line_string(row);
            text.push_str(if tight {
                next.as_str()
            } else {
                next.trim_start()
            });
        }
        let joined = to - from;
        let after = start + text.chars().count();
        self.replace_range(start, end, &text, Some(after.saturating_sub(1)));
        joined
    }

    /// Write the buffer out, and remember that what is on disk is what is being
    /// looked at.
    ///
    /// The one place a write happens, so that `:w` and a language server writing
    /// what it formatted cannot drift apart: both go through here, and both leave
    /// the saved marker where a person would expect it.
    pub fn write_buffer(&mut self, path: Option<&Path>) -> Result<PathBuf, BufferError> {
        let path = self.buffer.save(path)?;
        // The state that is on disk now is the one being looked at: say so, so
        // undoing away from it and back to it keeps the marker honest.
        self.history.mark_saved();
        Ok(path)
    }

    /// Write the lines an address range covers to a file, and say how many there
    /// were.
    ///
    /// This does not mark the buffer saved: what went to disk is part of it, and
    /// the rest is still unsaved — which is the difference between `:w` and
    /// `:1,5w somewhere`.
    /// Add the lines of an address range to a file, and say how many there were.
    ///
    /// The difference from [`Editor::write_range`] is one call: a file that is
    /// opened to be appended to keeps what it had, and one that is written is
    /// replaced. Both walk the same lines, which is why the text of a range is
    /// built in one place.
    pub fn append_range(&self, from: usize, to: usize, path: &Path) -> Result<usize, BufferError> {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        let (from, to) = (from.min(to), from.max(to));
        let mut text = String::new();
        for row in from..=to {
            text.push_str(&self.buffer.line_string(row));
            text.push_str(&self.buffer.line_terminator(row));
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(text.as_bytes())?;
        Ok(to - from + 1)
    }

    /// The lines of a range, terminators and all — what `:w` writes out.
    fn range_text(&self, from: usize, to: usize) -> String {
        let mut text = String::new();
        for row in from..=to {
            text.push_str(&self.buffer.line_string(row));
            text.push_str(&self.buffer.line_terminator(row));
        }
        text
    }

    /// Write the lines of an address range to a file, and say how many there
    /// were.
    ///
    /// This does not mark the buffer saved: what went to disk is part of it, and
    /// the rest is still unsaved — which is the difference between `:w` and
    /// `:1,5w somewhere`.
    pub fn write_range(&self, from: usize, to: usize, path: &Path) -> Result<usize, BufferError> {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        let (from, to) = (from.min(to), from.max(to));
        std::fs::write(path, self.range_text(from, to))?;
        Ok(to - from + 1)
    }

    /// Sort the lines an address range covers, and say how many there were.
    ///
    /// The range is rewritten as one edit — the lines' own terminators and all,
    /// so the file's line endings survive and the whole thing is one step to
    /// undo — and the cursor goes to the first line of it, which is where a
    /// person looks afterwards.
    pub fn sort_lines(&mut self, from: usize, to: usize, spec: &SortSpec) -> usize {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        let (from, to) = (from.min(to), from.max(to));
        let mut lines: Vec<String> = (from..=to)
            .map(|row| self.buffer.line_string(row))
            .collect();
        if spec.unique {
            // Equal by the same rule the sort uses, keeping the first of each.
            let mut seen: Vec<String> = Vec::new();
            lines.retain(|line| {
                let key = spec.key(line);
                if seen.contains(&key) {
                    false
                } else {
                    seen.push(key);
                    true
                }
            });
        }
        lines.sort_by_key(|line| spec.key(line));
        if spec.reverse {
            lines.reverse();
        }
        let count = lines.len();
        let mut text = String::new();
        for (index, line) in lines.iter().enumerate() {
            text.push_str(line);
            // The terminator of the line being replaced in this position, so a
            // file whose last line has none still has none.
            text.push_str(&self.buffer.line_terminator(from + index));
        }
        let start = self.buffer.line_col_to_char(from, 0);
        let end = self.buffer.line_col_to_char(to, 0)
            + self.buffer.line_len(to)
            + self.buffer.line_terminator_len(to);
        self.replace_range(start, end, &text, Some(start));
        count
    }

    /// The mark a `:` line's range names that was never set, if there is one.
    ///
    /// Vim stops a command whose range names a mark that is not there rather
    /// than acting on a line nobody asked for, and so does this: the message is
    /// the same one the mark keys give.
    pub fn unset_range_mark(&self, line: &str) -> Option<char> {
        let (from, to) = crate::substitute::address_range(line);
        [from, to]
            .into_iter()
            .flatten()
            .find_map(|address| match address {
                Address::Mark(letter) if self.marks.get(letter).is_none() => Some(letter),
                _ => None,
            })
    }

    /// Run a `:` command line.
    pub fn execute_cmdline(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if let Some(letter) = self.unset_range_mark(line) {
            self.message = format!("E20: Mark not set: {letter}");
            return;
        }
        // `:s/…/…/`, `:%s/…/…/g` and their friends: a substitute rather than a
        // command whose name happens to start with an `s`.
        if Substitute::looks_like(line) {
            // A substitute carries its range *in the line*, so this is where the
            // manual's rule has to be applied: "for Ex commands that operate on buffer
            // lines, the range is adjusted to always start at the first line of a closed
            // fold and end at the last line of a closed fold. Thus `:s/foo/bar/g` when
            // used with the cursor on a closed fold will replace 'foo' with 'bar' in all
            // lines of the fold."
            let line = self.substitute_line_with_fold_range(line);
            self.run_command("substitute", &[Param::Str(line)]);
            return;
        }
        // Likewise `:g/…/…` and `:v/…/…`, which `:goto` and `:vsplit` are not.
        if crate::global::Global::looks_like(line) {
            match crate::global::Global::parse(line) {
                Ok(spec) => self.run_global(&spec),
                Err(error) => self.message = error.to_string(),
            }
            return;
        }
        // An address or a range in front of a command that acts on lines: `:5d`,
        // `:'a,'bd`. Other commands ignore it, which is what vim does with a
        // range they have no use for — `:1,5w` writes the file, not five lines.
        let range = self.resolve_range(line);
        let line = crate::substitute::strip_range(line).unwrap_or(line);
        let (mut cmd, mut arg) = match line.split_once(char::is_whitespace) {
            Some((cmd, arg)) => (cmd, arg.trim()),
            None => (line, ""),
        };
        // `:tab split` and `:tab new` are two words for the same things as
        // `:tabsplit` and `:tabnew`.
        // `:vertical resize 10` is two words for `:verticalresize`, the same
        // way `:tab split` is two words for one command.
        if cmd == "vertical" {
            match arg.split_once(char::is_whitespace) {
                Some(("resize" | "res", rest)) => {
                    cmd = "vresize";
                    arg = rest.trim();
                }
                None if arg == "resize" || arg == "res" => {
                    cmd = "vresize";
                    arg = "";
                }
                _ => {}
            }
        }
        if cmd == "tab" {
            match arg.split_once(char::is_whitespace) {
                Some((sub, rest)) => {
                    cmd = match sub {
                        "split" => "tabsplit",
                        "new" | "edit" => "tabnew",
                        _ => "tab",
                    };
                    arg = rest.trim();
                }
                None if arg == "split" => {
                    cmd = "tabsplit";
                    arg = "";
                }
                None => {}
            }
        }
        let params = if arg.is_empty() {
            Vec::new()
        } else {
            vec![Param::Str(arg.to_string())]
        };
        // `:{range}write >> {file}`: part of the buffer, added to a file.
        if matches!(cmd, "w" | "write") && arg.trim_start().starts_with(">>") {
            let rest = arg.trim_start().trim_start_matches(">>").trim();
            let Some(path) = rest.split_whitespace().next().map(PathBuf::from) else {
                self.message = "E471: Argument required".to_string();
                return;
            };
            let last = self.buffer.line_count().saturating_sub(1);
            let (from, to) = range.unwrap_or((0, last));
            match self.append_range(from, to, &path) {
                Ok(count) => {
                    self.message = format!(
                        "{count} line{} appended to \"{}\"",
                        if count == 1 { "" } else { "s" },
                        path.display()
                    );
                }
                Err(error) => self.message = error.to_string(),
            }
            return;
        }
        // `:{range}write {file}`: part of the buffer, to a file.
        if matches!(cmd, "w" | "write") && range.is_some() {
            let Some(path) = arg.split_whitespace().next().map(PathBuf::from) else {
                self.message = "E471: Argument required".to_string();
                return;
            };
            let last = self.buffer.line_count().saturating_sub(1);
            let (from, to) = range.unwrap_or((0, last));
            match self.write_range(from, to, &path) {
                Ok(count) => {
                    self.message = format!(
                        "{count} line{} written to \"{}\"",
                        if count == 1 { "" } else { "s" },
                        path.display()
                    );
                }
                Err(error) => self.message = error.to_string(),
            }
            return;
        }
        // `:read {file}`: the file's lines, below the one the cursor is on — or
        // below the range's last line, when the line named one.
        if matches!(cmd, "r" | "read") {
            let Some(path) = arg.split_whitespace().next().map(PathBuf::from) else {
                self.message = "E471: Argument required".to_string();
                return;
            };
            if let Some((_, to)) = range {
                let last = self.buffer.line_count().saturating_sub(1);
                self.cursor.row = to.min(last);
            }
            match self.read_file_below(&path) {
                Ok(count) => {
                    self.message = format!(
                        "{count} line{} read from \"{}\"",
                        if count == 1 { "" } else { "s" },
                        path.display()
                    );
                }
                Err(error) => self.message = error.to_string(),
            }
            return;
        }
        // `:normal {keys}`: the keys as if typed, once per line of the range.
        //
        // The keys are what is left of the line *verbatim* — a space in them is
        // a key, not a separator — and they are read with the same notation
        // everything else uses, so `<Esc>` in them is the Escape key rather than
        // five characters. A run that is still inserting when its keys end is
        // put back into normal mode, which is what makes the old idiom
        // `:%normal A;` append to every line rather than to the first.
        if cmd == "normal" || cmd == "norm" {
            let Some(keys) = line.split_once(char::is_whitespace).map(|(_, keys)| keys) else {
                self.message = "E471: Argument required".to_string();
                return;
            };
            let last = self.buffer.line_count().saturating_sub(1);
            let (from, to) =
                range.unwrap_or((self.cursor.row.min(last), self.cursor.row.min(last)));
            let count = to.saturating_sub(from) + 1;
            for row in from..=to {
                if row > self.buffer.line_count().saturating_sub(1) {
                    break;
                }
                let at = self.buffer.line_col_to_char(row, 0);
                self.cursor.set_char_idx(&self.buffer, at);
                for key in crate::keymap::parse_normal_keys(keys) {
                    self.handle_key(key);
                }
                if self.mode == Mode::Insert {
                    self.run_command("escape", &[]);
                }
            }
            self.message = format!(
                "{count} line{} normalised",
                if count == 1 { "" } else { "s" }
            );
            return;
        }
        // `:{range}format` is the server's formatting, for the lines a range
        // names; `:format` with no range is the whole file.
        if matches!(cmd, "format" | "fmt") && range.is_some() {
            let last = self.buffer.line_count().saturating_sub(1);
            let (from, to) = range.unwrap_or((0, last));
            self.run_command(
                "format_range_command",
                &[Param::Int(from as i64), Param::Int(to as i64)],
            );
            return;
        }
        // `:sort` and its flags: the whole file when no range was named, and one
        // edit however many lines it moves.
        if let Some(flags) = cmd.strip_prefix("sort") {
            let last = self.buffer.line_count().saturating_sub(1);
            // The range was read before the line was stripped of it.
            let (from, to) = range.unwrap_or((0, last));
            let mut spec = SortSpec::default();
            // `:sort!` has no space between the name and the flag, and
            // `:sort n` has one: they are the same list either way.
            let flags = format!("{flags}{}", arg.trim());
            for flag in flags.chars() {
                match flag {
                    '!' => spec.reverse = true,
                    'n' => spec.numeric = true,
                    'i' => spec.ignore_case = true,
                    'u' => spec.unique = true,
                    other => {
                        self.message = format!("E474: Invalid argument: {other}");
                        return;
                    }
                }
            }
            let count = self.sort_lines(from, to, &spec);
            self.message = format!("{count} line{} sorted", if count == 1 { "" } else { "s" });
            return;
        }
        // A delete with a range is that many lines rather than the cursor's one.
        if matches!(cmd, "d" | "delete") {
            if let Some((from, to)) = range {
                let start = self.buffer.line_col_to_char(from, 0);
                let end = self.buffer.line_col_to_char(to, 0)
                    + self.buffer.line_len(to)
                    + self.buffer.line_terminator_len(to);
                self.cut(start, end, Kind::Linewise, Some(start));
                return;
            }
        }
        let name = match cmd {
            "d" | "delete" => "delete_line",
            "marks" => "list_marks",
            "delmarks" => "delete_marks",
            "delmarks!" => "delete_all_marks",
            "jumps" => "list_jumps",
            "changes" => "list_changes",
            "symbols" => "list_symbols",
            "signature" => "signature_help",
            "cnext" | "cn" => "place_next",
            "cprev" | "cp" => "place_previous",
            "clist" => "place_list",
            "copen" => "open_places",
            "cclose" => "close_places",
            "colder" => "list_older",
            "cnewer" => "list_newer",
            "format" | "fmt" => "format_document",
            "wsymbols" | "ws" => "workspace_symbols",
            "lopen" => "open_diagnostics",
            "lclose" => "close_diagnostics",
            "clearjumps" => "clear_jumps",
            "p" | "print" => "print_line",
            "w" => "save",
            "w!" => "save_force",
            "q" => "quit",
            "wq" | "x" => "save_quit",
            "q!" => "quit_force",
            "e" => "edit_file",
            "e!" => "edit_file_force",
            "ls" | "buffers" | "files" => "list_buffers",
            "b" | "buffer" => "buffer_go",
            "bn" | "bnext" => "buffer_next",
            "bp" | "bprevious" => "buffer_previous",
            "bd" | "bdelete" => "buffer_delete",
            "bn!" | "bnext!" => "buffer_next_force",
            "bp!" | "bprevious!" => "buffer_previous_force",
            "bd!" | "bdelete!" => "buffer_delete_force",
            "sp" | "split" => "split_rows",
            "vs" | "vsplit" => "split_columns",
            "on" | "only" => "window_only",
            "reg" | "registers" => "list_registers",
            "diagnostics" | "diags" => "list_diagnostics",
            "setdiag" => "set_diagnostics",
            // `set {name}={value}`, and `set {name}?` to ask. The options layer
            // already knows every name and every kind of value; this is the
            // shortest way to reach it from the `:` line.
            "set" => {
                let (name, value) = match arg.split_once('=') {
                    Some((name, value)) => (name.trim().to_string(), value.trim().to_string()),
                    None => (arg.trim().to_string(), String::new()),
                };
                // `:set all` is every setting in a window of its own, and
                // `:set inv{name}` turns a switch the other way — vim's two ways of
                // looking at the whole set and of flipping one of it.
                if name == "all" && value.is_empty() {
                    self.open_settings_window();
                    return;
                }
                if value.is_empty() {
                    if let Some(rest) = name.strip_prefix("inv") {
                        match crate::settings::Setting::from_name(rest) {
                            None => {
                                self.message = format!("E518: Unknown option: {rest}");
                            }
                            Some(setting) => {
                                let flipped = match self.options.borrow().get(setting) {
                                    Param::Bool(value) => (!value).to_string(),
                                    _ => {
                                        self.message = format!("E474: {rest} is not a switch");
                                        return;
                                    }
                                };
                                match self.set_option(setting, &flipped) {
                                    Ok(()) => self.message = format!("{rest}={flipped}"),
                                    Err(error) => {
                                        self.message = format!("E474: {rest} {error}");
                                    }
                                }
                            }
                        }
                        return;
                    }
                }
                // The three spellings vim has, since they are the three anybody
                // types: `:set number` turns a switch on, `:set nonumber` turns it
                // off, and `:set number?` asks what it is. A bare name that is not
                // a switch asks too, there being no "on" for a number.
                let asked = name.ends_with('?');
                let (name, value) = match (asked, value.is_empty(), name.strip_prefix("no")) {
                    (false, true, Some(rest)) => match Setting::from_name(rest) {
                        Some(setting)
                            if matches!(self.options.borrow().get(setting), Param::Bool(_)) =>
                        {
                            (rest.to_string(), "false".to_string())
                        }
                        _ => (name, value),
                    },
                    (false, true, None) => match Setting::from_name(&name) {
                        Some(setting)
                            if matches!(self.options.borrow().get(setting), Param::Bool(_)) =>
                        {
                            (name, "true".to_string())
                        }
                        _ => (name, value),
                    },
                    _ => (name, value),
                };
                let name = name.trim_end_matches('?').to_string();
                match crate::settings::Setting::from_name(&name) {
                    None => self.message = format!("E518: Unknown option: {name}"),
                    Some(setting) if value.is_empty() => {
                        self.message = format!("{name}={}", self.option_text(setting));
                    }
                    Some(setting) => match self.set_option(setting, &value) {
                        Ok(()) => {
                            self.message = format!("{name}={value}");
                            // With `'foldmethod'` set to `indent` the folds are a
                            // function of the settings — the method itself, the
                            // characters ignored, how deep they may nest, and what a
                            // tab counts as — so setting *any* of them works the folds
                            // out again. A `:set` is not a keystroke, and a promise of
                            // folds is not folds.
                            if self.foldmethod_is_indent() {
                                let count = self.compute_indent_folds();
                                self.message = format!(
                                    "{name}={value} ({count} fold{})",
                                    if count == 1 { "" } else { "s" }
                                );
                            }
                        }
                        Err(error) => self.message = format!("E474: {name} {error}"),
                    },
                }
                return;
            }
            "mkview" => "make_view",
            "loadview" => "load_view",
            "tabnew" | "tabedit" => "tab_new",
            "tabclose" | "tabc" => "tab_close",
            "tabnext" | "tabn" => "tab_next",
            "tabprevious" | "tabp" => "tab_previous",
            "tabs" => "list_tabs",
            "resize" | "res" => "resize",
            "vresize" => "resize_vertical",
            "tabsplit" => "tab_split",
            "&" => "substitute_again",
            "&&" => "substitute_again_flags",
            other => {
                self.message = format!("E492: Not a command: {other}");
                return;
            }
        };
        self.run_command(name, &params);
    }

    /// Remember where a jump started, so `<C-o>` and `''` can come back to it.
    ///
    /// Only the keys that travel call this: a motion within a line is not a jump,
    /// and neither is a scroll.
    pub fn record_jump(&mut self) {
        self.jumps.record(self.cursor);
        // `''` is not a walk through the list: it is the one place the last jump
        // started, so that pressing it twice comes back.
        self.jump_from = Some(self.cursor);
    }

    /// Run `:g` or `:v`: a command on every line a pattern matches (or every
    /// line it does not).
    ///
    /// The lines are worked through from the bottom up, because the command is
    /// usually one that takes lines out — `:g/TODO/d` — and removing a line does
    /// not move the ones above it. A command that prints is served by the message
    /// row's one line rather than by running it per line: the matching lines are
    /// collected into that line, the way `:diagnostics` lists what it has.
    pub fn run_global(&mut self, spec: &crate::global::Global) {
        let pattern = match regex::RegexBuilder::new(&spec.pattern).build() {
            Ok(pattern) => pattern,
            Err(error) => {
                self.message = crate::search::PatternError::from(error).to_string();
                return;
            }
        };
        let last = self.buffer.line_count().saturating_sub(1);
        let resolve = |address: crate::substitute::Address| self.resolve_address(address).min(last);
        let (from, to) = match (spec.from, spec.to) {
            (None, _) => (0, last),
            (Some(from), Some(to)) => {
                let (from, to) = (resolve(from), resolve(to));
                (from.min(to), from.max(to))
            }
            (Some(from), None) => (resolve(from), resolve(from)),
        };
        let rows: Vec<usize> = (from..=to)
            .filter(|row| {
                let line = self.buffer.line_string(*row);
                pattern.is_match(&line) != spec.invert
            })
            .collect();
        if rows.is_empty() {
            self.message = format!("Pattern not found: {}", spec.pattern);
            return;
        }
        let command = spec.command.trim();
        if command.is_empty() || command == "p" || command == "print" {
            let mut parts = vec![format!("{} lines", rows.len())];
            for row in rows.iter().take(8) {
                parts.push(format!(
                    "{} {}",
                    row + 1,
                    self.buffer.line_string(*row).trim()
                ));
            }
            if rows.len() > 8 {
                parts.push(format!("... {} more", rows.len() - 8));
            }
            self.message = parts.join("   ");
            return;
        }
        // Whatever each run has to say is left standing: a command that fails
        // says why, and only when there was nothing to say does the count show.
        self.message.clear();
        for row in rows.iter().rev() {
            self.cursor
                .set_char_idx(&self.buffer, self.buffer.line_col_to_char(*row, 0));
            self.execute_cmdline(command);
        }
        if self.message.is_empty() {
            self.message = format!("{} lines", rows.len());
        }
    }

    /// The default `init.lua` location, if the platform has one.
    pub fn default_init_path() -> Option<PathBuf> {
        LuaHost::default_init_path()
    }
}

/// The byte index of the `chars`-th character of `text`, or its end.
///
/// The command line is a `String` and its cursor is a character index, the way
/// every other position in the editor is; this is where the two meet.
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
mod tests {
    use super::*;
    use crate::keymap::parse_key;
    use inkstone_rpc::{ATTR_BOLD, ATTR_UNDERLINE};

    fn feed(editor: &mut Editor, notation: &str) {
        for key in parse_key(notation).unwrap() {
            editor.handle_key(key);
        }
    }

    #[test]
    fn an_insert_mapping_of_several_keys_fires() {
        let mut ed = Editor::for_tests();
        ed.keymap
            .borrow_mut()
            .bind(Mode::Insert, "jk", vec!["escape".into()])
            .unwrap();
        feed(&mut ed, "ijk");
        assert_eq!(ed.buffer.text(), "\n", "the mapped keys typed nothing");
        assert_eq!(ed.mode, Mode::Normal, "and the mapping ran");
    }

    #[test]
    fn a_sequence_that_is_not_a_mapping_is_typed_after_all() {
        let mut ed = Editor::for_tests();
        ed.keymap
            .borrow_mut()
            .bind(Mode::Insert, "jk", vec!["escape".into()])
            .unwrap();
        feed(&mut ed, "ijx");
        assert_eq!(
            ed.buffer.text(),
            "jx\n",
            "the held-back key is typed when the next one proves the sequence"
        );
        assert_eq!(ed.mode, Mode::Insert, "and the mapping did not run");
    }

    #[test]
    fn a_held_back_key_is_replayed_the_way_it_was_pressed() {
        let mut ed = Editor::for_tests();
        ed.keymap
            .borrow_mut()
            .bind(Mode::Insert, "jk", vec!["escape".into()])
            .unwrap();
        // `j` then a backspace: not a mapping, so `j` is typed and the
        // backspace deletes it, in that order.
        feed(&mut ed, "ij<BS>x");
        assert_eq!(ed.buffer.text(), "x\n");
        assert_eq!(ed.mode, Mode::Insert);
    }

    #[test]
    fn a_key_bound_on_its_own_runs_on_its_own() {
        let mut ed = Editor::for_tests();
        {
            let mut keymap = ed.keymap.borrow_mut();
            keymap
                .bind(Mode::Insert, "j", vec!["escape".into()])
                .unwrap();
            keymap
                .bind(Mode::Insert, "jk", vec!["escape".into()])
                .unwrap();
        }
        // The editor has no clock, so nothing waits to see whether a second key
        // is coming: a key that is bound on its own is answered at once, and the
        // longer sequence behind it cannot fire. A user who wants `jk` should
        // not also bind `j`.
        feed(&mut ed, "ij");
        assert_eq!(ed.mode, Mode::Normal, "the one-key mapping ran");
        assert_eq!(ed.buffer.text(), "\n");
    }

    #[test]
    fn a_mapping_clears_the_keys_it_was_holding() {
        let mut ed = Editor::for_tests();
        ed.keymap
            .borrow_mut()
            .bind(Mode::Insert, "jk", vec!["escape".into()])
            .unwrap();
        feed(&mut ed, "ijk");
        assert!(ed.insert_pending.is_empty());
        feed(&mut ed, "ihello<Esc>");
        assert_eq!(ed.buffer.text(), "hello\n", "and typing works again");
    }

    #[test]
    fn the_normal_modes_multi_key_binding_still_works() {
        // The two pending mechanisms are separate: `dd` is a normal-mode
        // sequence, and it is not confused by the insert-mode one.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        feed(&mut ed, "dd");
        assert_eq!(ed.buffer.text(), "two\n");
        assert!(ed.insert_pending.is_empty());
    }

    #[test]
    fn typing_in_insert_mode_edits_the_buffer() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "ihello<Esc>");
        assert_eq!(ed.buffer.text(), "hello\n");
        assert_eq!(ed.mode, Mode::Normal);
        assert_eq!(ed.cursor, Cursor::new(0, 4), "escape steps back one column");
        assert!(ed.is_dirty());
        assert!(!ed.message.contains("unmapped"), "{}", ed.message);
    }

    #[test]
    fn escape_only_steps_back_when_leaving_insert() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "ii");
        assert_eq!(ed.buffer.text(), "i\n");
        feed(&mut ed, "<Esc>");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
        // Pressing escape again in normal mode leaves the cursor alone.
        feed(&mut ed, "<Esc>");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
    }

    #[test]
    fn enter_and_backspace_in_insert_mode() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "iab<CR>cd<Esc>");
        assert_eq!(ed.buffer.text(), "ab\ncd\n");
        assert_eq!(ed.cursor, Cursor::new(1, 1));

        // Backspace eats the character before the cursor, one grapheme at a time.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        ed.cursor = Cursor::new(0, 2);
        feed(&mut ed, "i<BS><Esc>");
        assert_eq!(ed.buffer.text(), "ac\n");
        assert_eq!(
            ed.cursor,
            Cursor::new(0, 0),
            "insert mode ended, so the cursor stepped back"
        );
        feed(&mut ed, "li<BS><Esc>");
        assert_eq!(ed.buffer.text(), "c\n");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
    }

    #[test]
    fn backspace_at_the_start_of_a_line_joins_lines() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("ab\ncd\n");
        ed.cursor = Cursor::new(1, 0);
        feed(&mut ed, "i<BS><Esc>");
        assert_eq!(ed.buffer.text(), "abcd\n");
        assert_eq!(ed.cursor, Cursor::new(0, 1));
    }

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
    fn toggle_case_walks_over_what_it_changed() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("heLLo world\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "~");
        assert_eq!(ed.buffer.text(), "HeLLo world\n");
        assert_eq!(ed.cursor.col, 1, "and moves on");
        // A count, wiping out the case of a word — and back again.
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "5~");
        assert_eq!(ed.buffer.text(), "hEllO world\n", "five characters");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "5~");
        assert_eq!(ed.buffer.text(), "HeLLo world\n", "toggled, not set");
        // Non-letters are left alone, and the end of the buffer is the end.
        ed.buffer = Buffer::from_text("1a2b\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "99~");
        assert_eq!(ed.buffer.text(), "1A2B\n", "the terminator is not a letter");
        assert_eq!(ed.cursor.col, 3, "and the cursor stays on the line");
    }

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
    fn marks_are_put_down_and_gone_back_to() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("first line\n    indented line\nthird\n");
        // `m` takes the next letter, and the cursor keeps its place.
        ed.cursor = Cursor::new(1, 6);
        feed(&mut ed, "ma");
        assert_eq!(
            ed.marks.get('a'),
            Some(Cursor::new(1, 6)),
            "where it was put"
        );
        assert_eq!(ed.cursor, Cursor::new(1, 6));
        // `` `a `` goes back to the exact character, `'a` to the line's first
        // non-blank one.
        ed.cursor = Cursor::new(2, 0);
        feed(&mut ed, "`a");
        assert_eq!(ed.cursor, Cursor::new(1, 6));
        ed.cursor = Cursor::new(2, 0);
        feed(&mut ed, "'a");
        assert_eq!(ed.cursor, Cursor::new(1, 4), "first non-blank");
        // A mark is a motion: `d'a` takes whole lines, its own line included,
        // and `` d`a `` the characters up to the one the mark is on.
        ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
        ed.cursor = Cursor::new(2, 4);
        feed(&mut ed, "mz");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "d'z");
        assert_eq!(ed.buffer.text(), "four\n", "lines one to three");
        ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
        ed.cursor = Cursor::new(2, 1);
        feed(&mut ed, "mz");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "d`z");
        assert_eq!(ed.buffer.text(), "hree\n", "up to the mark's character");
        // A mark nobody set says so, and the cursor stays.
        ed.buffer = Buffer::from_text("one\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "`q");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
        assert_eq!(ed.message, "E20: Mark not set: q");
        // The keys wait for a letter, and anything else calls them off.
        feed(&mut ed, "m");
        assert!(ed.mark_prefix.is_some());
        feed(&mut ed, "1");
        assert!(ed.mark_prefix.is_none(), "a digit is not a letter");
        feed(&mut ed, "`");
        feed(&mut ed, "<Esc>");
        assert!(ed.mark_prefix.is_none());
        // `:marks` lists what there is.
        ed.execute_cmdline("marks");
        assert!(
            ed.message.starts_with("2 marks   a 2:7   z 3:2"),
            "{}",
            ed.message
        );
    }

    #[test]
    fn a_mark_stays_with_its_document() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        ed.cursor = Cursor::new(1, 0);
        feed(&mut ed, "ma");
        // Another document has none of its own, and coming back finds it.
        ed.save_document();
        ed.documents.push(Document::new(
            Buffer::from_text("elsewhere\n"),
            Language::Plain,
        ));
        ed.windows[ed.current_window].document = ed.documents.len() - 1;
        ed.load_document();
        assert_eq!(ed.marks.get('a'), None, "a mark belongs to its file");
        ed.show_document(0);
        assert_eq!(ed.marks.get('a'), Some(Cursor::new(1, 0)));
    }

    #[test]
    fn a_range_can_name_a_mark() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\nfive\n");
        // `a` on line two, `b` on line four: `:'a,'b` is lines two to four.
        ed.cursor = Cursor::new(1, 0);
        feed(&mut ed, "ma");
        ed.cursor = Cursor::new(3, 0);
        feed(&mut ed, "mb");
        ed.execute_cmdline("'a,'bd");
        assert_eq!(ed.buffer.text(), "one\nfive\n", "the three lines between");
        // A substitute over the same range, and one address on its own, which
        // is that line and not the rest of the file.
        ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\nfive\n");
        ed.cursor = Cursor::new(1, 0);
        feed(&mut ed, "ma");
        ed.execute_cmdline("'a,$s/o/O/g");
        assert_eq!(
            ed.buffer.text(),
            "one\ntwO\nthree\nfOur\nfive\n",
            "from the mark to the end"
        );
        // The mark is on line two, and one address is that line: the `t` of
        // "three" is not touched.
        ed.execute_cmdline("'as/t/T/");
        assert_eq!(ed.buffer.text(), "one\nTwO\nthree\nfOur\nfive\n");
        // `:g` over a mark's range.
        ed.buffer = Buffer::from_text("one\ntwo TODO\nthree\nfour TODO\n");
        ed.cursor = Cursor::new(1, 0);
        feed(&mut ed, "ma");
        ed.execute_cmdline("'a,2g/TODO/d");
        assert_eq!(
            ed.buffer.text(),
            "one\nthree\nfour TODO\n",
            "only inside it"
        );
        // A mark nobody set stops the command rather than pointing at a line
        // nobody asked for.
        ed.execute_cmdline("'z,'zd");
        assert_eq!(
            ed.buffer.text(),
            "one\nthree\nfour TODO\n",
            "nothing changed"
        );
        assert_eq!(ed.message, "E20: Mark not set: z");
    }

    #[test]
    fn the_last_jump_is_where_the_keys_came_from() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "mz");
        ed.cursor = Cursor::new(3, 2);
        feed(&mut ed, "`z");
        assert_eq!(ed.cursor, Cursor::new(0, 0), "to the mark");
        // `''` comes back to where that jump started, and again to toggle.
        feed(&mut ed, "''");
        assert_eq!(
            ed.cursor,
            Cursor::new(3, 0),
            "back to its line, first non-blank"
        );
        feed(&mut ed, "''");
        assert_eq!(ed.cursor, Cursor::new(0, 0), "and back again");
        // The exact one keeps the column, the line one goes to the first
        // non-blank.
        ed.buffer = Buffer::from_text("one\n    two\nthree\n");
        ed.cursor = Cursor::new(2, 1);
        feed(&mut ed, "ma");
        ed.cursor = Cursor::new(0, 2);
        feed(&mut ed, "`a");
        assert_eq!(ed.cursor, Cursor::new(2, 1));
        feed(&mut ed, "``");
        assert_eq!(ed.cursor, Cursor::new(0, 2), "the exact last jump");
        feed(&mut ed, "`a");
        feed(&mut ed, "''");
        assert_eq!(
            ed.cursor,
            Cursor::new(0, 0),
            "the first non-blank of its line"
        );
        // A search is a jump too.
        ed.buffer = Buffer::from_text("one two\none three\none four\n");
        ed.cursor = Cursor::new(0, 0);
        ed.execute_cmdline("s/one/x/");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "/three<CR>");
        assert_eq!(ed.cursor.row, 1);
        feed(&mut ed, "''");
        assert_eq!(ed.cursor.row, 0, "back to where the search started");
        // Nothing to go back to, and it says so.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        feed(&mut ed, "''");
        assert!(ed.message.contains("E20"), "{}", ed.message);
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
        // Something that cannot be done here says so rather than matching
        // something else.
        ed.execute_cmdline("%s/a\\zsb/x/");
        assert!(ed.message.contains("not supported"), "{}", ed.message);
    }

    #[test]
    fn the_jump_list_walks_back_and_forward() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\nfive\n");
        // Marks on three lines, and jumps between them: each jump leaves the
        // place it started in the list.
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "ma");
        ed.cursor = Cursor::new(2, 0);
        feed(&mut ed, "mb");
        ed.cursor = Cursor::new(4, 0);
        feed(&mut ed, "mc");
        ed.cursor = Cursor::new(4, 0);
        feed(&mut ed, "`a");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
        feed(&mut ed, "`b");
        assert_eq!(ed.cursor, Cursor::new(2, 0));
        // Back: where the last jump started, then the one before it.
        feed(&mut ed, "<C-o>");
        assert_eq!(
            ed.cursor,
            Cursor::new(0, 0),
            "where the second jump started"
        );
        feed(&mut ed, "<C-o>");
        assert_eq!(ed.cursor, Cursor::new(4, 0), "and the first one");
        // Forward again, and no further than the end.
        feed(&mut ed, "<C-i>");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
        feed(&mut ed, "<C-i>");
        assert_eq!(ed.cursor, Cursor::new(2, 0), "which is where we were");
        feed(&mut ed, "<C-i>");
        assert_eq!(ed.cursor, Cursor::new(2, 0), "the end of the list");
        assert!(ed.message.contains("E20"), "{}", ed.message);
        // `<Tab>` is the same key as `<C-i>`, which is what a terminal gives
        // for either of them.
        feed(&mut ed, "2<C-o>");
        assert_eq!(ed.cursor, Cursor::new(4, 0), "two back, in one go");
        feed(&mut ed, "<Tab>");
        assert_eq!(ed.cursor, Cursor::new(0, 0), "one forward");
        feed(&mut ed, "2<C-o>");
        assert_eq!(ed.cursor, Cursor::new(4, 0), "back again");
        // `:jumps` lists them — newest first, with the walk's place marked.
        ed.execute_cmdline("jumps");
        assert!(ed.message.contains("jumps"), "{}", ed.message);
        assert!(ed.message.contains('>'), "{}", ed.message);
    }

    #[test]
    fn marks_and_jumps_can_be_taken_away() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
        for (row, letter) in [(0usize, 'a'), (1, 'b'), (2, 'c'), (3, 'd')] {
            ed.cursor = Cursor::new(row, 0);
            feed(&mut ed, &format!("m{letter}"));
        }
        // One letter at a time, together or apart.
        ed.execute_cmdline("delmarks a");
        assert_eq!(ed.marks.get('a'), None);
        assert_eq!(ed.marks.get('b'), Some(Cursor::new(1, 0)), "only that one");
        assert_eq!(ed.message, "1 mark deleted");
        ed.execute_cmdline("delmarks b c");
        assert_eq!(ed.marks.iter().count(), 1, "`d` is what is left");
        // A run of them, both ends included — and they do not have to be there.
        ed.cursor = Cursor::new(0, 0);
        for letter in ['a', 'b', 'c'] {
            feed(&mut ed, &format!("m{letter}"));
        }
        ed.execute_cmdline("delmarks a-c");
        assert_eq!(ed.marks.get('a'), None);
        assert_eq!(ed.marks.get('b'), None);
        assert_eq!(ed.marks.get('c'), None);
        assert_eq!(ed.marks.get('d'), Some(Cursor::new(3, 0)), "untouched");
        assert_eq!(ed.message, "3 marks deleted");
        // Something that is not a letter is named back rather than guessed at.
        ed.execute_cmdline("delmarks A");
        assert!(ed.message.contains("E474"), "{}", ed.message);
        ed.execute_cmdline("delmarks a-");
        assert_eq!(ed.marks.iter().count(), 1, "a hyphen with nothing after it");
        // Everything at once.
        ed.execute_cmdline("delmarks!");
        assert!(ed.marks.is_empty());
        // And the jump list, which `:jumps` then finds nothing in.
        ed.execute_cmdline("mz");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "`z");
        feed(&mut ed, "<C-o>");
        assert!(!ed.jumps.is_empty());
        ed.execute_cmdline("clearjumps");
        assert!(ed.jumps.is_empty());
        ed.execute_cmdline("jumps");
        assert_eq!(ed.message, "No jumps");
        feed(&mut ed, "<C-o>");
        assert!(ed.message.contains("E20"), "{}", ed.message);
    }

    #[test]
    fn the_change_list_walks_where_edits_were_made() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
        // Three edits, on three different lines.
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "x");
        ed.cursor = Cursor::new(2, 0);
        feed(&mut ed, "x");
        ed.cursor = Cursor::new(3, 0);
        feed(&mut ed, "x");
        assert_eq!(ed.buffer.text(), "ne\ntwo\nhree\nour\n");
        // `g;` walks back through them, `g,` forward again.
        feed(&mut ed, "g;");
        assert_eq!(ed.cursor.row, 2);
        feed(&mut ed, "g;");
        assert_eq!(ed.cursor.row, 0);
        feed(&mut ed, "g;");
        assert_eq!(ed.cursor.row, 0, "nothing older");
        assert!(ed.message.contains("no older change"), "{}", ed.message);
        feed(&mut ed, "g,");
        assert_eq!(ed.cursor.row, 2);
        feed(&mut ed, "2g,");
        assert_eq!(ed.cursor.row, 3, "a count walks that many at once");
        feed(&mut ed, "g,");
        assert!(ed.message.contains("no newer change"), "{}", ed.message);
        // `:changes` lists them with the line each edit is on now.
        ed.execute_cmdline("changes");
        assert!(ed.message.contains("3 changes"), "{}", ed.message);
        assert!(ed.message.contains("ne"), "{}", ed.message);
        // An insert session is one change on one line, however many keys it
        // took: the list is per line, not per keystroke.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("\n\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "ithree words<Esc>");
        assert_eq!(ed.buffer.text(), "three words\n\n");
        ed.execute_cmdline("changes");
        assert!(ed.message.starts_with("1 changes"), "{}", ed.message);
    }

    #[test]
    fn the_item_the_list_moves_to_is_the_one_asked_about() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "i");
        ed.type_text("o");
        ed.set_completions(vec![
            CompletionItem {
                label: "one".to_string(),
                insert: "one".to_string(),
                detail: String::new(),
                snippet: false,
            },
            CompletionItem {
                label: "other".to_string(),
                insert: "other".to_string(),
                detail: String::new(),
                snippet: false,
            },
        ]);
        // Opening the list selects one, and that one is worth asking about.
        let asked = ed.take_completion_resolve();
        assert!(asked.is_some(), "an item was selected");
        assert!(ed.take_completion_resolve().is_none(), "and taken once");
        // Moving to the next asks about that one instead.
        feed(&mut ed, "<C-n>");
        let next = ed.take_completion_resolve().expect("the next item");
        assert_ne!(Some(next.clone()), asked, "a different item");
        // What comes back lands on both copies of the item and asks for a frame.
        ed.redraw = false;
        ed.set_completion_detail(&next, "a word the server knows");
        assert!(ed.redraw, "the popup changed, so a frame is due");
        let state = ed.completion.as_ref().expect("a list is up");
        let shown = state
            .shown
            .iter()
            .find(|item| item.label == next)
            .expect("the shown copy");
        assert_eq!(shown.detail, "a word the server knows");
        let all = state
            .all
            .iter()
            .find(|item| item.label == next)
            .expect("the copy of everything");
        assert_eq!(all.detail, "a word the server knows");
        // Something nobody sent is not invented.
        ed.redraw = false;
        ed.set_completion_detail("nothing", "made up");
        assert!(!ed.redraw, "an item that is not there is not made up");
    }

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
    fn the_choice_a_stop_offers_can_be_picked_from() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "i");
        ed.set_completions(vec![CompletionItem {
            label: "s".to_string(),
            insert: "let ${1|one,two,three|} = ${2:value};".to_string(),
            detail: String::new(),
            snippet: true,
        }]);
        // The first of the choices is what is typed, and accepting the item
        // puts the list away: what is left is a snippet on its own.
        assert_eq!(ed.buffer.line_string(0), "let one = value;");
        feed(&mut ed, "<C-y>");
        assert!(ed.completion.is_none(), "the list is away");
        // Into the first stop, and then `<C-n>`/`<C-p>` pick another, in place.
        feed(&mut ed, "<Tab>");
        feed(&mut ed, "<C-n>");
        assert_eq!(ed.buffer.line_string(0), "let two = value;");
        feed(&mut ed, "<C-n>");
        assert_eq!(ed.buffer.line_string(0), "let three = value;");
        feed(&mut ed, "<C-n>");
        assert_eq!(ed.buffer.line_string(0), "let one = value;", "round again");
        feed(&mut ed, "<C-p>");
        assert_eq!(ed.buffer.line_string(0), "let three = value;", "and back");
        // `<Tab>` still walks the stops, and the last one moved with the text:
        // `three` is longer than `one`, and `value` is still where it belongs.
        feed(&mut ed, "<Tab>");
        assert_eq!(ed.buffer.line_string(0), "let three = value;");
        let (from, to) = ed.selected_range().expect("the second stop is selected");
        assert_eq!(
            ed.buffer.slice_text(from, to),
            "value",
            "the second stop followed the first one's growth"
        );
        feed(&mut ed, "<Tab>");
        assert!(ed.snippet_stops.is_empty(), "and then the session is over");
        // A stop with nothing to choose from takes no key: the list's keys mean
        // what they always meant.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "i");
        ed.set_completions(vec![CompletionItem {
            label: "s".to_string(),
            insert: "plain ${1:word}".to_string(),
            detail: String::new(),
            snippet: true,
        }]);
        feed(&mut ed, "<Tab>");
        let before = ed.buffer.line_string(0);
        assert!(
            !ed.snippet_choose(true),
            "one option is not something to pick from"
        );
        assert_eq!(ed.buffer.line_string(0), before, "and the text stands");
    }

    #[test]
    fn a_place_written_twice_says_what_the_first_one_says() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "i");
        ed.set_completions(vec![CompletionItem {
            label: "swap".to_string(),
            insert: "let ${1:name} = ${1:name};".to_string(),
            detail: String::new(),
            snippet: true,
        }]);
        assert_eq!(ed.buffer.line_string(0), "let name = name;");
        feed(&mut ed, "<C-y>");
        // The second writing is a mirror: the walk steps to the place once.
        assert_eq!(ed.snippet_stops.len(), 2);
        assert!(ed.snippet_stops[1].mirror, "the second is a mirror");
        feed(&mut ed, "<Tab>");
        assert_eq!(ed.cursor.char_idx(&ed.buffer), 7, "at the end of the place");
        assert!(ed.selection.is_some(), "with its text selected");
        // Typing replaces what is selected, and the mirror says the same.
        ed.type_text("value");
        assert_eq!(ed.buffer.line_string(0), "let value = value;");
        assert!(ed.selection.is_none(), "the selection went with it");
        // And the walk goes on from there rather than stopping at the mirror.
        feed(&mut ed, "<Tab>");
        assert!(ed.snippet_stops.is_empty(), "one place, and then over");
    }

    #[test]
    fn typing_over_a_placeholder_replaces_it() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "i");
        ed.set_completions(vec![CompletionItem {
            label: "s".to_string(),
            insert: "let ${1:name} = 1;".to_string(),
            detail: String::new(),
            snippet: true,
        }]);
        feed(&mut ed, "<C-y>");
        assert_eq!(ed.buffer.line_string(0), "let name = 1;");
        // `<Tab>` selects the placeholder, and what is typed replaces it rather
        // than going in beside it.
        feed(&mut ed, "<Tab>");
        assert!(ed.selection.is_some(), "the placeholder is selected");
        ed.type_text("value");
        assert_eq!(ed.buffer.line_string(0), "let value = 1;");
        assert!(ed.selection.is_none(), "and the selection is spent");
        // One `u` takes back the whole insert session — the snippet that was
        // accepted and the typing over its placeholder are one visit to insert
        // mode, and a visit is one step. (The placeholder is not a step of its
        // own: it never existed outside the session.)
        feed(&mut ed, "<Esc>");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.line_string(0), "", "the session, all of it");
    }

    #[test]
    fn lines_can_be_sorted() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("banana\napple\ncherry\n");
        ed.execute_cmdline("sort");
        assert_eq!(ed.buffer.text(), "apple\nbanana\ncherry\n");
        assert_eq!(ed.message, "3 lines sorted");
        // The other way round, and a range smaller than the file.
        ed.execute_cmdline("sort!");
        assert_eq!(ed.buffer.text(), "cherry\nbanana\napple\n");
        ed.execute_cmdline("2,3sort");
        assert_eq!(
            ed.buffer.text(),
            "cherry\napple\nbanana\n",
            "only two of them"
        );
        // One edit, however many lines moved: one `u` puts it all back.
        let before = ed.buffer.text();
        ed.execute_cmdline("sort");
        assert_ne!(ed.buffer.text(), before);
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), before);
        // Numbers compare as numbers: `9` before `10`.
        ed.buffer = Buffer::from_text("10 ten\n9 nine\n2 two\n");
        ed.execute_cmdline("sort n");
        assert_eq!(ed.buffer.text(), "2 two\n9 nine\n10 ten\n");
        // Without `n` they compare as text, which is the other order.
        ed.execute_cmdline("sort!");
        ed.execute_cmdline("sort");
        assert_eq!(ed.buffer.text(), "10 ten\n2 two\n9 nine\n");
        // Case can be ignored, and one of each kept.
        ed.buffer = Buffer::from_text("Beta\nalpha\nALPHA\nbeta\n");
        ed.execute_cmdline("sort iu");
        assert_eq!(ed.buffer.text(), "alpha\nBeta\n", "no case, no repeats");
        // A flag vim does not have is named back rather than ignored.
        ed.execute_cmdline("sort z");
        assert!(ed.message.contains("E474"), "{}", ed.message);
        // Sorting does not change how the file ends: the terminators are the
        // ones already in the range, so a file that ends without a line break
        // still does.
        ed.buffer = Buffer::from_text("b\na\nz");
        let newlines = ed.buffer.text().matches('\n').count();
        let ended = ed.buffer.text().ends_with('\n');
        ed.execute_cmdline("sort");
        assert_eq!(ed.buffer.text().matches('\n').count(), newlines);
        assert_eq!(ed.buffer.text().ends_with('\n'), ended);
        assert!(ed.buffer.text().starts_with("a\nb\nz"));
        // And a one-line file is a file nothing happens to.
        ed.buffer = Buffer::from_text("only\n");
        ed.execute_cmdline("sort");
        assert_eq!(ed.buffer.text(), "only\n");
        assert_eq!(ed.message, "1 line sorted");
    }

    #[test]
    fn keys_can_be_run_over_a_range_of_lines() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
        // The old idiom: append a semicolon to every line.
        ed.execute_cmdline("%normal A;");
        assert_eq!(ed.buffer.text(), "one;\ntwo;\nthree;\n");
        assert_eq!(ed.message, "3 lines normalised");
        // A range, and a key that is not a letter.
        ed.execute_cmdline("2,3normal ^x");
        assert_eq!(
            ed.buffer.text(),
            "one;\nwo;\nhree;\n",
            "two of them, one key each"
        );
        // `<Esc>` is the Escape key here, in the notation everything else uses.
        ed.execute_cmdline("%normal 0i> <Esc>A <");
        assert_eq!(ed.buffer.text(), "> one; <\n> wo; <\n> hree; <\n");
        // One line is one line, and no line is no line.
        ed.buffer = Buffer::from_text("only\n");
        ed.execute_cmdline("normal x");
        assert_eq!(ed.buffer.text(), "nly\n");
        assert_eq!(ed.message, "1 line normalised");
        // A name that is not a key is the characters it looks like, rather than
        // an error: a person writing `<` meant `<`.
        ed.buffer = Buffer::from_text("ab\n");
        ed.execute_cmdline("%normal i<notakey><Esc>0");
        assert_eq!(ed.buffer.text(), "<notakey>ab\n");
        // Spaces in the keys are keys, not separators.
        ed.buffer = Buffer::from_text("ab\n");
        ed.execute_cmdline("%normal i x");
        assert_eq!(ed.buffer.text(), " xab\n", "the space was typed");
    }

    #[test]
    fn a_file_can_be_read_below_a_line_and_part_of_one_written_out() {
        let dir = std::env::temp_dir().join("inkstone-read-write");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.txt");
        std::fs::write(&source, "alpha\nbeta\n").unwrap();
        let target = dir.join("target.txt");
        let _ = std::fs::remove_file(&target);

        // `:read` puts the file's lines below the cursor's own, as lines.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        ed.cursor = Cursor::new(0, 0);
        ed.execute_cmdline(&format!("read {}", source.display()));
        assert_eq!(ed.buffer.text(), "one\nalpha\nbeta\ntwo\n");
        assert!(ed.message.starts_with("2 lines read"), "{}", ed.message);
        // Read at the very end, where the last line has no terminator to sit
        // after: the file still comes in as its own lines.
        ed.buffer = Buffer::from_text("only");
        ed.cursor = Cursor::new(0, 0);
        ed.execute_cmdline(&format!("read {}", source.display()));
        assert_eq!(ed.buffer.text(), "only\nalpha\nbeta\n");
        // A file that is not there says so rather than reading nothing.
        ed.execute_cmdline(&format!("read {}", dir.join("nope.txt").display()));
        assert_eq!(ed.buffer.text(), "only\nalpha\nbeta\n");
        assert!(!ed.message.is_empty(), "{}", ed.message);

        // `:{range}write {file}` writes those lines and no others.
        ed.buffer = Buffer::from_text("one\ntwo\nthree\nfour\n");
        ed.execute_cmdline(&format!("2,3w {}", target.display()));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "two\nthree\n");
        assert!(ed.message.starts_with("2 lines written"), "{}", ed.message);
        // Writing part of a buffer does not make the whole of it saved.
        assert!(ed.is_dirty(), "the rest is still unsaved");
        // One line, and no argument at all.
        ed.execute_cmdline(&format!("1w {}", target.display()));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "one\n");
        ed.execute_cmdline("2w");
        assert!(ed.message.contains("E471"), "{}", ed.message);
        // `:w >> file` adds to what is there instead of replacing it.
        std::fs::write(&target, "already\n").unwrap();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        ed.execute_cmdline(&format!("w >> {}", target.display()));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "already\none\ntwo\n",
            "and the whole file when no range was named"
        );
        ed.execute_cmdline(&format!("1w >> {}", target.display()));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "already\none\ntwo\none\n",
            "or the part of it a range named"
        );
        assert!(ed.message.starts_with("1 line appended"), "{}", ed.message);
        // `:{range}read` puts the lines after that line rather than the
        // cursor's.
        ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
        ed.cursor = Cursor::new(0, 0);
        std::fs::write(&target, "read-me\n").unwrap();
        ed.execute_cmdline(&format!("2read {}", target.display()));
        assert_eq!(ed.buffer.text(), "one\ntwo\nread-me\nthree\n");
        // `:w` with no range is still the whole file — and `:w!` is what it
        // takes, since this test wrote that file itself and the buffer that is
        // saving has never read it.
        ed.buffer = Buffer::from_text("whole\n");
        ed.execute_cmdline(&format!("w {}", target.display()));
        assert!(
            ed.message.contains("E13"),
            "a file that changed on disk is not overwritten without asking: {}",
            ed.message
        );
        ed.execute_cmdline(&format!("w! {}", target.display()));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "whole\n");
        assert!(!ed.is_dirty(), "and that *is* the buffer being saved");
    }

    #[test]
    fn the_places_a_question_named_can_be_walked() {
        let dir = std::env::temp_dir().join("inkstone-places");
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("one.txt");
        let two = dir.join("two.txt");
        std::fs::write(&one, "first\nsecond\n").unwrap();
        std::fs::write(&two, "third\nfourth\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("mine\n");
        // With nothing asked, there is nothing to walk.
        ed.execute_cmdline("cnext");
        assert!(ed.message.contains("E776"), "{}", ed.message);
        // A question fills the list, and the walk starts at the first place.
        ed.set_places(vec![
            (one.clone(), 0, 0),
            (one.clone(), 1, 2),
            (two.clone(), 1, 0),
        ]);
        assert_eq!(ed.place_index, 1);
        ed.execute_cmdline("clist");
        assert!(ed.message.starts_with("3 places"), "{}", ed.message);
        assert!(ed.message.contains(">1 one.txt:1"), "{}", ed.message);
        // `:cnext` walks forwards and goes to the place it reaches — opening the
        // file if it is another one.
        ed.execute_cmdline("cnext");
        assert_eq!(ed.place_index, 2);
        assert_eq!(ed.cursor.row, 1);
        assert_eq!(ed.cursor.col, 2);
        ed.execute_cmdline("cnext");
        assert_eq!(ed.place_index, 3);
        assert!(
            ed.buffer.path().is_some_and(|path| path == two),
            "the third place is in the other file: {:?}",
            ed.buffer.path()
        );
        assert_eq!(ed.cursor.row, 1);
        // Off the end it stops and says so; `:cprev` walks back.
        ed.execute_cmdline("cnext");
        assert!(ed.message.contains("E553"), "{}", ed.message);
        assert_eq!(ed.place_index, 3, "and stays where it was");
        ed.execute_cmdline("cprev");
        assert_eq!(ed.place_index, 2);
        assert!(ed.buffer.path().is_some_and(|path| path == one));
        // A count walks that many at once, and off the front it stops too.
        ed.execute_cmdline("cprev");
        ed.execute_cmdline("2cprev");
        assert!(ed.message.contains("E553"), "{}", ed.message);
        assert_eq!(ed.place_index, 1, "still the first");
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
    fn a_setting_can_be_set_from_the_colon_line() {
        let mut ed = Editor::for_tests();
        ed.execute_cmdline("set textwidth=20");
        assert_eq!(ed.options.borrow().textwidth.0, 20);
        assert_eq!(ed.message, "textwidth=20");
        // A number has no "on", so a bare name asks — and `?` asks for both
        // kinds, which is what a vim-shaped editor owes anyone who types it.
        ed.execute_cmdline("set textwidth");
        assert_eq!(ed.message, "textwidth=20");
        assert_eq!(ed.options.borrow().textwidth.0, 20, "and changes nothing");
        ed.execute_cmdline("set textwidth?");
        assert_eq!(ed.message, "textwidth=20");
        ed.execute_cmdline("set number?");
        assert_eq!(ed.message, "number=false", "the switch is off");
        // A switch that is named alone is turned *on*, which is what `:set
        // number` means everywhere else.
        ed.execute_cmdline("set number");
        assert!(ed.options.borrow().number, "{}", ed.message);
        assert_eq!(ed.message, "number=true");
        ed.execute_cmdline("set nonumber");
        assert!(!ed.options.borrow().number);
        assert_eq!(ed.message, "number=false");
        ed.execute_cmdline("set nope?");
        assert!(ed.message.contains("E518"), "{}", ed.message);
        // The values a script writes are the values here.
        ed.execute_cmdline("set number=true");
        assert!(ed.options.borrow().number);
        ed.execute_cmdline("set number=false");
        assert!(!ed.options.borrow().number);
        // A number that does not fit the kind of setting says so.
        ed.execute_cmdline("set textwidth=-1");
        assert!(ed.message.starts_with("E474"), "{}", ed.message);
        assert_eq!(ed.options.borrow().textwidth.0, 20, "and leaves it alone");
        ed.execute_cmdline("set number=perhaps");
        assert!(ed.message.starts_with("E474"), "{}", ed.message);
        // And an option nobody has is named back.
        ed.execute_cmdline("set nope=1");
        assert!(ed.message.contains("E518"), "{}", ed.message);
    }

    #[test]
    fn gq_keeps_what_every_line_had_in_front_of_it() {
        let mut ed = Editor::for_tests();
        ed.options.borrow_mut().textwidth = crate::settings::Count(24);
        ed.buffer = Buffer::from_text("// one two three\n// four five six\n");
        feed(&mut ed, "gqG");
        assert_eq!(
            ed.buffer.text(),
            "// one two three four\n// five six\n",
            "the marker stays on both lines"
        );
        // A quotation, which has the same shape.
        ed.buffer = Buffer::from_text("> alpha beta gamma\n> delta epsilon\n");
        feed(&mut ed, "gqG");
        assert_eq!(ed.buffer.text(), "> alpha beta gamma delta\n> epsilon\n");
        // Indentation is a leader too, and lines that disagree have none.
        ed.buffer = Buffer::from_text("    one two three four five\n    six seven\n");
        feed(&mut ed, "gqG");
        assert_eq!(
            ed.buffer.text(),
            "    one two three four\n    five six seven\n"
        );
        ed.buffer = Buffer::from_text("one two three four five six\n  seven eight\n");
        feed(&mut ed, "gqG");
        assert_eq!(
            ed.buffer.text(),
            "one two three four five\nsix seven eight\n",
            "nothing shared, nothing kept"
        );
    }

    #[test]
    fn a_line_breaks_while_it_is_being_typed() {
        let mut ed = Editor::for_tests();
        ed.options.borrow_mut().textwidth = crate::settings::Count(12);
        ed.buffer = Buffer::from_text("\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "i");
        for c in "one two three four".chars() {
            ed.type_text(&c.to_string());
        }
        assert_eq!(
            ed.buffer.text(),
            "one two\nthree four\n",
            "broken at the last space before the width"
        );
        // A word longer than the width is not broken, because there is nowhere
        // to break it.
        let mut ed = Editor::for_tests();
        ed.options.borrow_mut().textwidth = crate::settings::Count(6);
        ed.buffer = Buffer::from_text("\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "i");
        ed.type_text("abcdefghij");
        assert_eq!(ed.buffer.text(), "abcdefghij\n", "no space, no break");
        // And zero is still "do not wrap".
        let mut ed = Editor::for_tests();
        ed.options.borrow_mut().textwidth = crate::settings::Count(0);
        ed.buffer = Buffer::from_text("\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "i");
        ed.type_text("one two three four five six");
        assert_eq!(ed.buffer.text(), "one two three four five six\n");
    }

    #[test]
    fn a_boolean_setting_can_be_turned_off_by_name() {
        let mut ed = Editor::for_tests();
        ed.options.borrow_mut().number = true;
        ed.execute_cmdline("set nonumber");
        assert!(!ed.options.borrow().number);
        assert_eq!(ed.message, "number=false");
        // `no` on something that is not a switch is not a name at all.
        ed.execute_cmdline("set notabstop");
        assert!(ed.message.contains("E518"), "{}", ed.message);
        assert_eq!(ed.options.borrow().tabstop, 4, "and nothing changed");
    }

    #[test]
    fn the_list_of_places_can_be_shown_in_a_window() {
        let dir = std::env::temp_dir().join("inkstone-copen");
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("one.txt");
        let two = dir.join("two.txt");
        std::fs::write(&one, "first\nsecond\n").unwrap();
        std::fs::write(&two, "third\nfourth\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("mine\n");
        // With no list there is nothing to open, and no window appears.
        ed.execute_cmdline("copen");
        assert!(ed.message.contains("E776"), "{}", ed.message);
        assert_eq!(ed.windows.len(), 1);

        ed.set_places(vec![(one.clone(), 0, 0), (two.clone(), 1, 1)]);
        ed.execute_cmdline("copen");
        assert_eq!(ed.windows.len(), 2, "a window of its own");
        assert_eq!(ed.list, Some(List::Places), "and it is a list, not a file");
        let text = ed.buffer.text();
        assert!(text.starts_with("1 "), "{text:?}");
        assert!(text.contains("one.txt"), "{text:?}");
        assert_eq!(ed.message, "2 places");
        // The lines are the places, in order: two places, two lines.
        assert_eq!(ed.buffer.line_count(), 2);

        // A list is not edited.
        let before = ed.buffer.text();
        feed(&mut ed, "x");
        assert_eq!(ed.buffer.text(), before);
        assert!(ed.message.starts_with("E21"), "{}", ed.message);

        // `<CR>` on a line goes to the place that line names, and closes the
        // window it was reading.
        feed(&mut ed, "j");
        feed(&mut ed, "<CR>");
        assert_eq!(ed.windows.len(), 1, "the list window went away");
        assert_eq!(ed.list, None);
        assert!(
            ed.buffer.path().is_some_and(|path| path == two),
            "and we are at the second place: {:?}",
            ed.buffer.path()
        );
        assert_eq!(ed.cursor.row, 1);
        assert_eq!(ed.cursor.col, 1);

        // `:copen` again, and `:cclose` closes it without going anywhere.
        ed.execute_cmdline("copen");
        assert_eq!(ed.windows.len(), 2);
        ed.execute_cmdline("cclose");
        assert_eq!(ed.windows.len(), 1);
        // Anywhere but a list, `<CR>` is the motion it always was.
        assert_eq!(ed.list, None);
        ed.execute_cmdline("cclose");
        assert!(ed.message.contains("E776"), "{}", ed.message);
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
    fn a_new_line_starts_where_the_last_one_did() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("    one two\nthree\n");
        // Off by default, as in vim.
        ed.cursor = Cursor::new(0, 8);
        feed(&mut ed, "o");
        assert_eq!(ed.buffer.text(), "    one two\n\nthree\n");
        assert_eq!(ed.cursor, Cursor::new(1, 0), "at the start, not indented");
        feed(&mut ed, "<Esc>");
        ed.buffer = Buffer::from_text("    one two\nthree\n");
        ed.cursor = Cursor::new(0, 8);
        ed.options.borrow_mut().autoindent = true;
        assert!(ed.options.borrow().autoindent, "the setting is on");
        // `o` puts the new line below, with the same indentation, and the cursor
        // after it.
        assert_eq!(ed.indent_of(0), "    ", "the indent is read");
        feed(&mut ed, "o");
        assert_eq!(ed.buffer.text(), "    one two\n    \nthree\n");
        assert_eq!(ed.cursor, Cursor::new(1, 4), "after the indentation");
        ed.type_text("more");
        assert_eq!(ed.buffer.text(), "    one two\n    more\nthree\n");
        feed(&mut ed, "<Esc>");
        // `O` puts it above, with the indentation of the line it was opened
        // from — which is the line the cursor is on, as in vim.
        ed.buffer = Buffer::from_text("    one two\nthree\n");
        ed.cursor = Cursor::new(0, 8);
        feed(&mut ed, "O");
        assert_eq!(ed.buffer.text(), "    \n    one two\nthree\n");
        assert_eq!(ed.cursor, Cursor::new(0, 4));
        feed(&mut ed, "<Esc>");
        // A line break typed in insert mode does it too, as one edit.
        ed.buffer = Buffer::from_text("    one\n");
        ed.cursor = Cursor::new(0, 7);
        feed(&mut ed, "a<CR>");
        assert_eq!(ed.buffer.text(), "    one\n    \n");
        assert_eq!(ed.cursor, Cursor::new(1, 4));
        feed(&mut ed, "<Esc>");
        feed(&mut ed, "u");
        assert_eq!(
            ed.buffer.text(),
            "    one\n",
            "the break and the indent together"
        );
        // A line with no indentation has none to copy.
        ed.buffer = Buffer::from_text("one\ntwo\n");
        ed.cursor = Cursor::new(0, 2);
        feed(&mut ed, "o");
        assert_eq!(ed.buffer.text(), "one\n\ntwo\n");
        assert_eq!(ed.cursor, Cursor::new(1, 0));
    }

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
        ed.buffer =
            Buffer::from_text("fn main() {\nlet x = 1;\nif x > 0 {\nprintln!(\"x\");\n}\n}\n");
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

    #[test]
    fn a_range_can_be_moved_a_level_in_or_out() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
        // `>>` moves one line in; a count moves that many.
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "<gt><gt>");
        assert_eq!(ed.buffer.text(), "\tone\ntwo\nthree\n");
        // A count is lines, as in vim: `2>>` moves two of them in.
        feed(&mut ed, "2<gt><gt>");
        assert_eq!(ed.buffer.text(), "\t\tone\n\ttwo\nthree\n");
        // `<<` takes a level off, and one edit takes it all back.
        ed.buffer = Buffer::from_text("\tone\n\t\ttwo\n\tthree\n");
        ed.cursor = Cursor::new(2, 0);
        feed(&mut ed, "<lt><lt>");
        assert_eq!(ed.buffer.text(), "\tone\n\t\ttwo\nthree\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "\tone\n\t\ttwo\n\tthree\n", "one edit");
        // A line indented with spaces loses a tab stop's worth, or all of them.
        ed.buffer = Buffer::from_text("  two spaces\n    four\n\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "<lt>j");
        assert_eq!(
            ed.buffer.text(),
            "two spaces\nfour\n\n",
            "two spaces is less than a stop, so all of them go"
        );
        // A blank line stays blank: indenting nothing is not indentation.
        ed.buffer = Buffer::from_text("\n");
        feed(&mut ed, "<gt><gt>");
        assert_eq!(ed.buffer.text(), "\n");
        // A motion, over the lines it covers.
        ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "<gt>j");
        assert_eq!(ed.buffer.text(), "\tone\n\ttwo\nthree\n");
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
    fn a_list_of_edits_is_applied_backwards_as_one_step() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
        // Two edits, given in document order, and both relative to the text as
        // it was: the later one must not move the earlier one's offsets.
        let edits = vec![(0, 3, "1".to_string()), (8, 13, "3".to_string())];
        assert_eq!(ed.apply_edits(&edits), 2);
        assert_eq!(ed.buffer.text(), "1\ntwo\n3\n");
        // One step to undo, however many edits there were.
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "one\ntwo\nthree\n", "all of it");
        // An edit that inserts just before the last line break — 13 is where
        // that break is — and one that deletes everything.
        let edits = vec![(13, 13, "!".to_string())];
        assert_eq!(ed.apply_edits(&edits), 1);
        assert_eq!(ed.buffer.text(), "one\ntwo\nthree!\n");
        // Deleting everything leaves the one empty line a buffer always has.
        let len = ed.buffer.len_chars();
        let edits = vec![(0, len, String::new())];
        assert_eq!(ed.apply_edits(&edits), 1);
        assert_eq!(
            ed.buffer.text(),
            "\n",
            "an empty document is one empty line"
        );
        // Nothing asked for is nothing done.
        assert_eq!(ed.apply_edits(&[]), 0);
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
    fn a_file_that_changed_underneath_is_not_overwritten_by_accident() {
        let dir = std::env::temp_dir().join("inkstone-stamp");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shared.txt");
        std::fs::write(&path, "mine\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.open_file(&path).unwrap();
        ed.type_text("more\n"); // at the cursor, which is the top of the file
                                // Something else writes the file: the next `:w` refuses.
        std::fs::write(&path, "theirs\n").unwrap();
        ed.execute_cmdline("w");
        assert!(
            ed.message.starts_with("E13"),
            "the write is refused: {}",
            ed.message
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "theirs\n",
            "and their text is still there"
        );
        // `:w!` says a person knows, and writes anyway.
        ed.execute_cmdline("w!");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "more\nmine\n");
        // And after writing, the stamp is the new one: another `:w` is fine.
        ed.execute_cmdline("w");
        assert!(
            ed.message.contains("written"),
            "nothing changed since: {}",
            ed.message
        );
        // A file that did not exist has nothing to compare with, so it saves.
        let fresh = dir.join("fresh.txt");
        let _ = std::fs::remove_file(&fresh);
        let mut ed = Editor::for_tests();
        ed.open_file(&fresh).unwrap();
        ed.type_text("hello"); // a new file is one empty line, so no break of its own
        ed.execute_cmdline("w");
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "hello\n");
    }

    #[test]
    fn the_lists_a_session_showed_can_be_walked_back_through() {
        let dir = std::env::temp_dir().join("inkstone-colder");
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("one.txt");
        let two = dir.join("two.txt");
        std::fs::write(&one, "a\n").unwrap();
        std::fs::write(&two, "b\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("mine\n");
        // Nothing shown yet.
        ed.execute_cmdline("colder");
        assert!(ed.message.contains("E776"), "{}", ed.message);
        // Two questions, so two lists.
        ed.set_places(vec![(one.clone(), 0, 0)]);
        ed.set_places(vec![(two.clone(), 0, 0), (two.clone(), 0, 0)]);
        assert_eq!(ed.list_history.len(), 2);
        assert_eq!(ed.list_index, 2, "on the newest");
        // `:copen` then `:colder` shows the older one, in a window of its own.
        ed.execute_cmdline("copen");
        assert_eq!(ed.list_targets.len(), 2);
        ed.execute_cmdline("colder");
        assert_eq!(ed.list_index, 1);
        assert_eq!(ed.list_targets.len(), 1, "the older list");
        assert_eq!(ed.list_targets[0].0, one);
        assert_eq!(ed.windows.len(), 2, "one window, not two");
        assert!(ed.message.contains("1 of 2"), "{}", ed.message);
        // And back the other way.
        ed.execute_cmdline("cnewer");
        assert_eq!(ed.list_index, 2);
        assert_eq!(ed.list_targets.len(), 2);
        assert!(ed.message.contains("2 of 2"), "{}", ed.message);
        // The ends say so rather than wrapping.
        ed.execute_cmdline("cnewer");
        assert!(ed.message.contains("E553"), "{}", ed.message);
        assert_eq!(ed.list_index, 2);
        ed.execute_cmdline("colder");
        ed.execute_cmdline("colder");
        assert!(ed.message.contains("E553"), "{}", ed.message);
        assert_eq!(ed.list_index, 1);
        // The list the walk restored is the one `:cnext` walks.
        assert_eq!(ed.places.len(), 1);
    }

    #[test]
    fn a_wrapped_list_keeps_its_marker_on_one_line() {
        let mut ed = Editor::for_tests();
        ed.options.borrow_mut().textwidth = crate::settings::Count(20);
        // A bullet: the marker on the first line, spaces under the text after it.
        ed.buffer = Buffer::from_text("- one two three four\n- five six seven\n");
        feed(&mut ed, "gqG");
        assert_eq!(
            ed.buffer.text(),
            "- one two three four\n  five six seven\n",
            "the second line is lined up under the first's text"
        );
        // A number, which is wider. (One line, since `1. ` and `2. ` are not a
        // leader two lines share — the digits differ.)
        ed.buffer = Buffer::from_text("1. alpha beta gamma delta\n");
        feed(&mut ed, "gqG");
        assert_eq!(
            ed.buffer.text(),
            "1. alpha beta gamma\n   delta\n",
            "three spaces under the text"
        );
        // A quotation is not a list: its marker is repeated on every line, since
        // every line of a quotation is quoted.
        ed.buffer = Buffer::from_text("> one two three\n> four five six\n");
        feed(&mut ed, "gqG");
        assert_eq!(ed.buffer.text(), "> one two three four\n> five six\n");
        // And a leader that is only indentation is repeated — with the
        // indentation not counting towards the width, so sixteen columns of text
        // under four columns of indent.
        ed.buffer = Buffer::from_text("    one two three\n    four five\n");
        feed(&mut ed, "gqG");
        assert_eq!(
            ed.buffer.text(),
            "    one two three\n    four five\n",
            "the same lines, since sixteen columns is what fits"
        );
    }

    #[test]
    fn closing_a_list_window_takes_its_document_with_it() {
        let dir = std::env::temp_dir().join("inkstone-nolist");
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("one.txt");
        std::fs::write(&one, "a\n").unwrap();

        let mut ed = Editor::for_tests();
        // A real file, opened the way a person opens one: poking at the live
        // buffer would leave the *document slot* empty and the name would be
        // `[No Name]`, which is the line this project keeps meeting.
        ed.open_file(&one).unwrap();
        let documents = ed.documents.len();
        ed.set_places(vec![(one.clone(), 0, 0)]);
        // Opening and closing a list ten times leaves nothing behind.
        for _ in 0..10 {
            ed.execute_cmdline("copen");
            assert_eq!(ed.documents.len(), documents + 1, "the list is a document");
            assert_eq!(ed.windows.len(), 2);
            assert_eq!(ed.current_document(), documents, "and it is on screen");
            ed.execute_cmdline("cclose");
            assert_eq!(ed.windows.len(), 1);
            assert_eq!(
                ed.documents.len(),
                documents,
                "and closing the window took it away"
            );
        }
        // The window that is left still shows the file it did, and the file is
        // where it was.
        assert_eq!(ed.buffer.display_name(), "one.txt");
        assert!(!ed.is_dirty(), "and the file is untouched");
        // Closing a window on a *file* still just closes the window.
        ed.split_window(Axis::Rows, None);
        assert_eq!(ed.windows.len(), 2);
        ed.execute_cmdline("cclose");
        assert!(ed.message.contains("E776"), "{}", ed.message);
        assert_eq!(ed.documents.len(), documents, "the file stayed open");
        assert_eq!(ed.buffer.display_name(), "one.txt");
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
    fn gf_opens_the_file_named_under_the_cursor() {
        let dir = std::env::temp_dir().join("inkstone-gf");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("one.txt"), "the one\n").unwrap();
        std::fs::write(dir.join("src").join("two.txt"), "the two\n").unwrap();
        let here = dir.join("here.txt");
        std::fs::write(&here, "see src/two.txt and one.txt\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.open_file(&here).unwrap();
        // The name is a *path*, not a word: `src/two.txt` is three words and one
        // file name, and `gf` takes the whole of it.
        ed.cursor.row = 0;
        ed.cursor.col = 6;
        feed(&mut ed, "gf");
        assert_eq!(ed.buffer.display_name(), "two.txt");
        assert_eq!(
            ed.buffer.text(),
            "the two\n",
            "and it is the file that opened"
        );
        assert!(ed.message.contains("two.txt"), "{}", ed.message);

        // Relative to the file that is open, which is what a name in a file
        // means. Back to it, and the plain name in the same line.
        ed.open_file(&here).unwrap();
        ed.cursor.row = 0;
        ed.cursor.col = 22;
        feed(&mut ed, "gf");
        assert_eq!(ed.buffer.display_name(), "one.txt");
        assert_eq!(ed.buffer.text(), "the one\n");

        // A name that is not there says so, and changes nothing.
        ed.open_file(&here).unwrap();
        let before = ed.buffer.display_name();
        ed.buffer = Buffer::from_text("see nowhere.txt\n");
        ed.cursor.row = 0;
        ed.cursor.col = 6;
        feed(&mut ed, "gf");
        assert!(
            ed.message.starts_with("E484"),
            "a name that is not there is named back: {}",
            ed.message
        );
        assert_eq!(ed.buffer.text(), "see nowhere.txt\n", "and nothing opened");
        let _ = before;

        // `'path'` is where a name is looked for when it is not beside the file:
        // the setting is a comma-separated list, searched in order, which is how a
        // name in an `import` line finds a file in another directory.
        let elsewhere = dir.join("lib");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("helper.txt"), "the helper\n").unwrap();
        let mut ed = Editor::for_tests();
        ed.open_file(&here).unwrap();
        ed.buffer = Buffer::from_text("see helper.txt\n");
        ed.cursor.row = 0;
        ed.cursor.col = 6;
        feed(&mut ed, "gf");
        assert!(
            ed.message.starts_with("E484"),
            "not found yet: {}",
            ed.message
        );
        ed.execute_cmdline(&format!("set path={}", elsewhere.display()));
        assert_eq!(ed.options.borrow().path.0.len(), 1, "one directory");
        ed.buffer = Buffer::from_text("see helper.txt\n");
        ed.cursor.row = 0;
        ed.cursor.col = 6;
        feed(&mut ed, "gf");
        assert_eq!(ed.buffer.display_name(), "helper.txt");
        assert_eq!(ed.buffer.text(), "the helper\n");
        // A list is written with commas, and the empty parts are not directories.
        ed.execute_cmdline("set path=/one,,/two");
        assert_eq!(
            ed.options.borrow().path.0.len(),
            2,
            "two directories, and no empty one"
        );
        assert_eq!(ed.options.borrow().path.0[1].display().to_string(), "/two");

        // Punctuation with no name in it is not a file name.
        ed.buffer = Buffer::from_text("...\n");
        ed.cursor.row = 0;
        ed.cursor.col = 1;
        feed(&mut ed, "gf");
        assert!(ed.message.starts_with("E484"), "{}", ed.message);
        let _ = std::fs::remove_dir_all(&dir);
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
    fn set_all_shows_every_setting_and_set_inv_turns_one_around() {
        let mut ed = Editor::for_tests();
        let windows = ed.windows.len();
        let documents = ed.documents.len();
        ed.execute_cmdline("set all");
        assert_eq!(ed.windows.len(), windows + 1, "in a window of its own");
        assert_eq!(ed.documents.len(), documents + 1);
        assert_eq!(ed.list, Some(List::Settings), "and it is a list");
        let text = ed.buffer.text();
        for setting in crate::settings::Setting::ALL.iter().copied() {
            assert!(
                text.contains(setting.name()),
                "{} is in the list: {text}",
                setting.name()
            );
        }
        assert!(text.contains("tabstop"), "{text}");
        assert!(text.contains("scrolloff"), "{text}");
        assert!(ed.message.contains("settings"), "{}", ed.message);
        // A list is not for editing, which is the same rule every list follows.
        let before = ed.buffer.text();
        ed.type_text("x");
        assert!(ed.message.starts_with("E21"), "{}", ed.message);
        assert_eq!(ed.buffer.text(), before, "and nothing changed");
        // `:set inv{name}` turns a switch the other way, and says what it is now.
        ed.execute_cmdline("cclose");
        assert!(!ed.options.borrow().number);
        ed.execute_cmdline("set invnumber");
        assert!(ed.options.borrow().number);
        assert_eq!(ed.message, "number=true");
        ed.execute_cmdline("set invnumber");
        assert!(!ed.options.borrow().number);
        assert_eq!(ed.message, "number=false");
        // A setting that is not a switch has no other way to be.
        ed.execute_cmdline("set invtextwidth");
        assert!(
            ed.message.starts_with("E474") && ed.message.contains("switch"),
            "{}",
            ed.message
        );
        assert_eq!(ed.options.borrow().textwidth.0, 79, "and it is left alone");
        ed.execute_cmdline("set invnope");
        assert!(ed.message.contains("E518"), "{}", ed.message);
    }

    #[test]
    fn q_colon_shows_the_history_and_runs_a_line_of_it() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        // Nothing typed yet, so there is nothing to show. (The keys are fed one
        // at a time: the notation writes a two-key sequence with a space, and
        // `parse_key` reads a space as part of it rather than as a separator.)
        feed(&mut ed, "q");
        feed(&mut ed, ":");
        assert!(ed.message.contains("E776"), "{}", ed.message);
        assert_eq!(ed.windows.len(), 1, "and no window was opened");

        // Two commands *typed*, which is what the history is of — a command run
        // from a script is not something a person typed, and does not go in it.
        let prompt = |ed: &mut Editor, opens: &str, line: &str| {
            feed(ed, opens);
            feed(ed, line);
            feed(ed, "<CR>");
        };
        prompt(&mut ed, ":", "set number");
        prompt(&mut ed, ":", "set nonumber");
        feed(&mut ed, "q");
        feed(&mut ed, ":");
        assert_eq!(ed.list, Some(List::Commands), "it is a list");
        assert_eq!(ed.windows.len(), 2);
        let text = ed.buffer.text();
        assert!(text.contains("set number\n"), "{text:?}");
        assert!(text.contains("set nonumber\n"), "{text:?}");
        assert!(text.find("set number").unwrap() < text.find("set nonumber").unwrap());
        // `<CR>` on a line runs it again, and the window goes away — so what the
        // command did is not half a screen down.
        ed.cursor.row = 0;
        ed.open_place_at_cursor();
        assert_eq!(ed.windows.len(), 1, "the window closed");
        assert!(ed.options.borrow().number, "and the line ran");

        // `q/` is the same for searches, which are typed on the same line.
        prompt(&mut ed, "/", "one");
        assert_eq!(
            ed.search_history,
            vec!["one".to_string()],
            "and it was typed"
        );
        feed(&mut ed, "q");
        feed(&mut ed, "/");
        assert_eq!(ed.list, Some(List::Searches));
        let text = ed.buffer.text();
        assert!(text.contains("one"), "{text:?}");
        ed.cursor.row = 0;
        ed.open_place_at_cursor();
        assert_eq!(ed.windows.len(), 1);
        // (The message row is the name of the document the window went back to —
        // closing a window says what is on screen, which is what `:e` does too.)
        assert_eq!(ed.search.pattern(), "one", "the search ran");
        assert_eq!(ed.cursor.row, 0, "and it went to the match");
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
    fn an_operator_can_be_forced_to_characters_or_lines() {
        // Vim's `o_v`: after an operator, `v` forces the motion to work
        // characterwise — a linewise motion becomes *exclusive*, and a
        // characterwise one has its inclusive/exclusive the other way round.
        const SOURCE: &str = "one two three\nfour five\n";
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(SOURCE);
        ed.cursor.row = 0;
        ed.cursor.col = 4;
        // `dj` would take two whole lines; `dvj` takes from the cursor to the
        // character below it, and not that character.
        feed(&mut ed, "dvj");
        assert_eq!(
            ed.buffer.text(),
            "one  five\n",
            "from the cursor up to — but not including — the character below it"
        );

        // `$` is inclusive, so `v` makes it exclusive: the last character stays.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(SOURCE);
        ed.cursor.row = 0;
        ed.cursor.col = 4;
        feed(&mut ed, "dv$");
        assert_eq!(
            ed.buffer.text(),
            "one e\nfour five\n",
            "everything but the last character"
        );

        // `w` is exclusive, so `v` makes it inclusive: the next word's first
        // character goes too.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(SOURCE);
        ed.cursor.row = 0;
        ed.cursor.col = 4;
        feed(&mut ed, "dvw");
        assert_eq!(
            ed.buffer.text(),
            "one hree\nfour five\n",
            "one character further"
        );

        // `o_V` is the other way: `V` forces whole lines, whatever the motion
        // was. `dVw` takes the whole line the motion reaches.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(SOURCE);
        ed.cursor.row = 0;
        ed.cursor.col = 4;
        feed(&mut ed, "dVw");
        assert_eq!(ed.buffer.text(), "four five\n", "the whole line");

        // And the shape is spent by the motion it was given to: the next operator
        // is its own again.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(SOURCE);
        ed.cursor.row = 0;
        ed.cursor.col = 4;
        feed(&mut ed, "dvjd");
        assert_eq!(ed.forced_shape, None, "nothing is left forced");
        assert_eq!(
            ed.buffer.text(),
            "one  five\n",
            "and `d` is linewise again — `dj` takes what is left of two lines"
        );
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
    fn a_macro_records_and_plays() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
        ed.cursor.row = 0;
        // `qa` starts recording, `x` and `j` go into it, and `q` ends it.
        feed(&mut ed, "qa");
        assert_eq!(ed.message, "recording @a");
        feed(&mut ed, "xj");
        feed(&mut ed, "q");
        assert_eq!(ed.message, "recorded @a");
        assert_eq!(
            ed.buffer.text(),
            "ne\ntwo\nthree\n",
            "the keys did their work"
        );
        assert_eq!(ed.macros[0].as_deref(), Some("xj"), "and only those keys");
        assert!(ed.recording.is_none());
        // Playing it does the same again, from where the cursor is.
        feed(&mut ed, "@a");
        assert_eq!(ed.buffer.text(), "ne\nwo\nthree\n");
        assert_eq!(
            ed.cursor.row, 2,
            "and the cursor is where the macro left it"
        );
        // A count plays it that many times, and `@@` plays the last one again.
        feed(&mut ed, "2@a");
        assert_eq!(ed.buffer.text(), "ne\nwo\nree\n");
        feed(&mut ed, "@@");
        assert_eq!(ed.buffer.text(), "ne\nwo\nee\n");
        // A register nobody recorded into says so, and does nothing.
        let before = ed.buffer.text();
        feed(&mut ed, "@z");
        assert!(ed.message.contains("E748"), "{}", ed.message);
        assert_eq!(ed.buffer.text(), before);

        // `q:` is still the history window: the letter-taking state hands the
        // pair the keymap names back to it.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        feed(&mut ed, ":");
        feed(&mut ed, "set number");
        feed(&mut ed, "<CR>");
        feed(&mut ed, "q");
        feed(&mut ed, ":");
        assert_eq!(ed.list, Some(List::Commands), "the window opened");
        assert!(ed.recording.is_none(), "and no recording was started");
    }

    #[test]
    fn j_joins_lines() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n    two\nthree\n");
        ed.cursor.row = 0;
        // `J` joins the cursor's line with the next, taking the next one's indent
        // out and putting a space in its place.
        feed(&mut ed, "J");
        assert_eq!(ed.buffer.text(), "one two\nthree\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "one\n    two\nthree\n", "one `u`");

        // A count joins that many lines, and one `u` takes the whole thing back.
        feed(&mut ed, "3J");
        assert_eq!(ed.buffer.text(), "one two three\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "one\n    two\nthree\n");

        // `'joinspaces'` is on, so a line that ends a sentence is joined with two
        // spaces — and with one when the option is off.
        let dir = std::env::temp_dir().join("inkstone-joinspaces");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sentences.txt");
        std::fs::write(&path, "One.\n    Two\nThree!\nFour\n").unwrap();
        let mut ed = Editor::for_tests();
        ed.open_file(&path).unwrap();
        assert!(ed.options.borrow().joinspaces, "on by default, as in vim");
        feed(&mut ed, "4J");
        assert_eq!(
            ed.buffer.text(),
            "One.  Two Three!  Four\n",
            "two spaces after each full stop, one after the other line"
        );
        feed(&mut ed, "u");
        assert_eq!(
            ed.buffer.text(),
            "One.\n    Two\nThree!\nFour\n",
            "one `u` takes the whole join back"
        );
        ed.execute_cmdline("set nojoinspaces");
        feed(&mut ed, "4J");
        assert_eq!(ed.buffer.text(), "One. Two Three! Four\n", "one space each");
        let _ = std::fs::remove_dir_all(&dir);

        // `gJ` inserts and removes no spaces at all: the indent of the next line
        // stays exactly where it was.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("One.\n    Two\nThree!\n");
        feed(&mut ed, "gJ");
        assert_eq!(ed.buffer.text(), "One.    Two\nThree!\n", "no space added");
        feed(&mut ed, "u");
        feed(&mut ed, "g3J");
        assert_eq!(ed.buffer.text(), "One.    TwoThree!\n", "and none anywhere");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "One.\n    Two\nThree!\n", "one `u`");

        // The last line has nothing below it to join to.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        ed.cursor.row = 1;
        feed(&mut ed, "J");
        assert!(ed.message.starts_with("E85"), "{}", ed.message);
        assert_eq!(ed.buffer.text(), "one\ntwo\n", "and nothing moved");

        // `vJ` joins every line of the selection, which is what vim's "always whole
        // lines" note means.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n    two\n        three\nfour\n");
        ed.cursor.row = 0;
        feed(&mut ed, "vjj");
        feed(&mut ed, "J");
        assert_eq!(ed.buffer.text(), "one two three\nfour\n");
        assert_eq!(ed.mode, Mode::Normal, "and visual mode is over");
        feed(&mut ed, "u");
        assert_eq!(
            ed.buffer.text(),
            "one\n    two\n        three\nfour\n",
            "one `u` for the whole join"
        );
    }

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
        ed.buffer = Buffer::from_text(
            "top\n    one\n        two\n\n        two again\n    one again\nback\n",
        );
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

    #[test]
    fn cursor_keys_work_too() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("ab\ncd\n");
        feed(&mut ed, "<Down><Right>");
        assert_eq!(ed.cursor, Cursor::new(1, 1));
        feed(&mut ed, "<Home>");
        assert_eq!(ed.cursor, Cursor::new(1, 0));
        feed(&mut ed, "<End>");
        assert_eq!(ed.cursor, Cursor::new(1, 1), "`<End>` is vim's `$`");
    }

    #[test]
    fn delete_char_and_delete_line() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\ndef\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "x");
        assert_eq!(ed.buffer.text(), "bc\ndef\n");
        feed(&mut ed, "dd");
        assert_eq!(ed.buffer.text(), "def\n");
        feed(&mut ed, "dd");
        assert_eq!(ed.buffer.text(), "\n");
        feed(&mut ed, "dd");
        assert_eq!(
            ed.buffer.text(),
            "\n",
            "the last line cannot be removed entirely"
        );
    }

    #[test]
    fn pasted_text_is_one_step() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        ed.cursor = Cursor::new(0, 0);
        // What a paste, or an input method's commit, hands the daemon.
        ed.type_text("a\npasted\nblock");
        assert_eq!(ed.buffer.text(), "a\npasted\nblockone\ntwo\n");
        assert_eq!(ed.cursor, Cursor::new(2, 5), "and the cursor follows it");

        ed.undo();
        assert_eq!(ed.buffer.text(), "one\ntwo\n", "one `u` takes it all back");
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
    fn the_register_remembers_what_a_delete_took() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one two\n");
        feed(&mut ed, "dw");
        assert_eq!(ed.buffer.text(), "two\n");
        // `dw` took characters, so `p` puts them back into the line, after the
        // character under the cursor.
        feed(&mut ed, "p");
        assert_eq!(ed.buffer.text(), "tone wo\n");
        feed(&mut ed, "P");
        assert_eq!(
            ed.buffer.text(),
            "toneone  wo\n",
            "`P` puts it back before the cursor, which `p` left on the space"
        );
    }

    #[test]
    fn a_yanked_line_goes_below_or_above() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        feed(&mut ed, "yy");
        assert_eq!(ed.buffer.text(), "one\ntwo\n", "yanking changes nothing");
        feed(&mut ed, "p");
        assert_eq!(ed.buffer.text(), "one\none\ntwo\n", "below the line");
        feed(&mut ed, "P");
        assert_eq!(ed.buffer.text(), "one\none\none\ntwo\n", "above it");
        // The last line is the interesting one: there is no line below it.
        feed(&mut ed, "Gyy");
        feed(&mut ed, "p");
        assert_eq!(ed.buffer.text(), "one\none\none\ntwo\ntwo\n");
    }

    #[test]
    fn a_yanked_line_keeps_its_own_terminator() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\r\ntwo\r\n");
        feed(&mut ed, "yyp");
        assert_eq!(
            ed.buffer.text(),
            "one\r\none\r\ntwo\r\n",
            "a CRLF line stays a CRLF line"
        );
    }

    #[test]
    fn a_put_is_one_step_to_undo() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        feed(&mut ed, "yyp");
        assert_eq!(ed.buffer.text(), "one\none\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "one\n", "one `u` takes the whole paste");
        assert_eq!(
            ed.register.text(),
            "one\n",
            "and the register still holds it"
        );
    }

    #[test]
    fn x_takes_the_character_but_an_insert_delete_does_not() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        feed(&mut ed, "xp");
        assert_eq!(ed.buffer.text(), "noe\n", "`x` fills the register");

        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        feed(&mut ed, "yy");
        feed(&mut ed, "i<Del><Esc>");
        assert_eq!(ed.buffer.text(), "ne\ntwo\n", "insert mode's delete edits");
        feed(&mut ed, "p");
        assert_eq!(
            ed.buffer.text(),
            "ne\none\ntwo\n",
            "and it left the yanked line where it was"
        );
    }

    #[test]
    fn change_takes_the_word_and_leaves_the_space() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one two\n");
        feed(&mut ed, "cw");
        assert_eq!(ed.mode, Mode::Insert);
        assert_eq!(ed.buffer.text(), " two\n", "`cw` is `ce` in disguise");
        feed(&mut ed, "1<Esc>");
        assert_eq!(ed.buffer.text(), "1 two\n");

        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        feed(&mut ed, "cc");
        assert_eq!(ed.mode, Mode::Insert);
        assert_eq!(
            ed.buffer.text(),
            "\ntwo\n",
            "`cc` leaves the line behind, empty — it is not `dd` then `i`"
        );
        feed(&mut ed, "x<Esc>");
        assert_eq!(ed.buffer.text(), "x\ntwo\n");

        // The same for a linewise change that takes more than one line.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
        feed(&mut ed, "cj");
        assert_eq!(ed.buffer.text(), "\nthree\n", "one line where two were");
        assert_eq!(ed.mode, Mode::Insert);
    }

    #[test]
    fn a_put_with_an_empty_register_changes_nothing() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        feed(&mut ed, "p");
        assert_eq!(ed.buffer.text(), "one\n");
        assert_eq!(ed.message, "");
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
        feed(&mut ed, "/\\zs");
        assert_eq!(ed.message, "");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
        feed(&mut ed, "<Esc>");

        // The line itself still reports both, when it is accepted.
        feed(&mut ed, "/zzz<Enter>");
        assert_eq!(ed.message, "E486: Pattern not found: zzz");
        feed(&mut ed, "/\\zs<Enter>");
        assert!(ed.message.starts_with("E54: "), "{}", ed.message);
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
    fn the_prompt_is_shown_where_the_command_line_is() {
        let mut ed = Editor::for_tests();
        ed.handle_resize(20, 4);
        feed(&mut ed, "/t");
        assert_eq!(
            ed.compose().grid.row_text(3).trim_end(),
            "/t",
            "the bottom row shows the prompt and what is typed"
        );
        feed(&mut ed, "<Esc>");
        assert!(ed.compose().grid.row_text(3).trim_end().is_empty());

        // A `:` command still spells itself with a colon.
        feed(&mut ed, ":w");
        assert_eq!(ed.compose().grid.row_text(3).trim_end(), ":w");
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
    fn a_line_can_be_edited_wherever_the_cursor_is() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        feed(&mut ed, "/abc");
        feed(&mut ed, "<Home>x");
        assert_eq!(ed.cmdline, "xabc", "a character goes where the cursor is");

        feed(&mut ed, "<End><BS>");
        assert_eq!(ed.cmdline, "xab");
        feed(&mut ed, "<Left>");
        assert_eq!(ed.cmdline_cursor, 2);
        feed(&mut ed, "<Del>");
        assert_eq!(
            ed.cmdline, "xa",
            "`<Del>` takes the character under the cursor"
        );
        feed(&mut ed, "<BS>");
        assert_eq!(ed.cmdline, "x", "and `<BS>` the one before it");
        feed(&mut ed, "<Home><Del>");
        assert_eq!(ed.cmdline, "");

        feed(&mut ed, "abc<Enter>");
        assert_eq!(ed.cursor, Cursor::new(0, 0), "and the edited pattern runs");
        assert_eq!(ed.message, "");
    }

    #[test]
    fn up_and_down_walk_the_history_and_bring_back_the_draft() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":nope<Enter>");
        feed(&mut ed, ":also-nope<Enter>");
        assert_eq!(ed.command_history, vec!["nope", "also-nope"]);

        feed(&mut ed, ":half");
        feed(&mut ed, "<Up>");
        assert_eq!(ed.cmdline, "also-nope");
        feed(&mut ed, "<Up>");
        assert_eq!(ed.cmdline, "nope");
        feed(&mut ed, "<Up>");
        assert_eq!(ed.cmdline, "nope", "the oldest line is as far as it goes");
        feed(&mut ed, "<Down>");
        assert_eq!(ed.cmdline, "also-nope");
        feed(&mut ed, "<Down>");
        assert_eq!(ed.cmdline, "half", "and the line being typed comes back");
        feed(&mut ed, "<Down>");
        assert_eq!(ed.cmdline, "half", "past it, it stays");
    }

    #[test]
    fn a_line_run_twice_is_remembered_once() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":nope<Enter>");
        feed(&mut ed, ":nope<Enter>");
        assert_eq!(ed.command_history, vec!["nope"]);
        feed(&mut ed, ":<Enter>");
        assert_eq!(
            ed.command_history,
            vec!["nope"],
            "an empty line is not kept"
        );
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
    fn escape_forgets_the_prompt_and_what_was_typed() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":nope<Enter>");
        feed(&mut ed, ":half<Up>");
        assert_eq!(ed.cmdline, "nope");
        feed(&mut ed, "<Esc>");
        assert_eq!(ed.cmdline, "");
        assert_eq!(ed.cmdline_cursor, 0);
        assert_eq!(ed.prompt_index, None);
        assert_eq!(ed.prompt_draft, "");
        assert_eq!(ed.command_history, vec!["nope"], "the history is kept");
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
    fn undo_and_redo_put_the_text_and_the_cursor_back() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        ed.cursor = Cursor::new(0, 1);
        feed(&mut ed, "x");
        assert_eq!(ed.buffer.text(), "ac\n");

        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "abc\n");
        assert_eq!(ed.cursor, Cursor::new(0, 1), "the cursor comes back too");

        feed(&mut ed, "<C-r>");
        assert_eq!(ed.buffer.text(), "ac\n");
        assert_eq!(ed.cursor, Cursor::new(0, 1));
    }

    #[test]
    fn an_insert_session_is_one_step() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "ihello<Esc>");
        assert_eq!(ed.buffer.text(), "hello\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "\n", "typing is one step, not five");
    }

    #[test]
    fn typing_and_backspacing_over_it_is_still_one_step() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "iabc<BS><Esc>");
        assert_eq!(ed.buffer.text(), "ab\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "\n", "the whole session comes back out");
    }

    #[test]
    fn a_line_opened_and_typed_into_is_one_step() {
        // `o` writes the line break and then the typing lands in front of it:
        // two edits, one thing the person did, so one `u`.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        feed(&mut ed, "ohello<Esc>");
        assert_eq!(ed.buffer.text(), "one\nhello\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "one\n", "the whole line came back out");
        feed(&mut ed, "<C-r>");
        assert_eq!(ed.buffer.text(), "one\nhello\n", "and redo puts it back");

        // The same for a line above, and for typing then backspacing over part
        // of it in a session that was opened that way.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        feed(&mut ed, "Oabove<Esc>");
        assert_eq!(ed.buffer.text(), "above\none\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "one\n");

        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        feed(&mut ed, "ohello<BS><BS><Esc>");
        assert_eq!(ed.buffer.text(), "one\nhel\n");
        feed(&mut ed, "u");
        assert_eq!(
            ed.buffer.text(),
            "one\n",
            "still one step after backspacing"
        );
    }

    #[test]
    fn changing_a_word_and_typing_the_new_one_is_one_step() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one two three\n");
        feed(&mut ed, "wcwX<Esc>");
        assert_eq!(ed.buffer.text(), "one X three\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "one two three\n", "the word is back");
        feed(&mut ed, "<C-r>");
        assert_eq!(ed.buffer.text(), "one X three\n");
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
    fn dot_repeats_a_change_that_took_a_character_find() {
        // The find is part of the motion the operator took, and the character it
        // looked for is remembered, so `.` finds its own comma again.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one, two, three, four\n");
        feed(&mut ed, "df,");
        assert_eq!(ed.buffer.text(), " two, three, four\n");
        feed(&mut ed, ".");
        assert_eq!(ed.buffer.text(), " three, four\n", "the next comma, again");
    }

    #[test]
    fn dot_repeats_the_last_change() {
        // A delete, repeated with a count.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abcdefgh\n");
        feed(&mut ed, "x");
        assert_eq!(ed.buffer.text(), "bcdefgh\n");
        feed(&mut ed, ".");
        assert_eq!(ed.buffer.text(), "cdefgh\n", "the same change again");
        feed(&mut ed, "3.");
        assert_eq!(ed.buffer.text(), "fgh\n", "and with a count");

        // A line delete, from wherever the cursor is now.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
        feed(&mut ed, "dd");
        assert_eq!(ed.buffer.text(), "two\nthree\n");
        feed(&mut ed, "j.");
        assert_eq!(ed.buffer.text(), "two\n", "the line under the cursor");

        // A motion in between does not count as a change.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("aa bb cc\n");
        feed(&mut ed, "x");
        assert_eq!(ed.buffer.text(), "a bb cc\n");
        feed(&mut ed, "w.");
        assert_eq!(
            ed.buffer.text(),
            "a b cc\n",
            "the change, where the cursor went"
        );

        // With nothing changed yet, `.` does nothing at all.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        feed(&mut ed, ".");
        assert_eq!(ed.buffer.text(), "one\n");
    }

    #[test]
    fn dot_repeats_the_text_an_insert_session_typed() {
        // `cw` and the text after it are one change.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one two three\n");
        feed(&mut ed, "wcwXXX<Esc>");
        assert_eq!(ed.buffer.text(), "one XXX three\n");
        feed(&mut ed, "w.");
        assert_eq!(ed.buffer.text(), "one XXX XXX\n", "the word and the text");

        // `i`, `a` and `o` carry their text along the same way.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        feed(&mut ed, "i[[<Esc>.");
        assert_eq!(ed.buffer.text(), "[[[[one\ntwo\n");
        feed(&mut ed, "jA]]<Esc>.");
        assert_eq!(ed.buffer.text(), "[[[[one\ntwo]]]]\n");
        feed(&mut ed, "joline<Esc>.");
        assert_eq!(ed.buffer.text(), "[[[[one\ntwo]]]]\nline\nline\n");

        // A backspace in the session is part of it, and `.` types the session's
        // keys again — so the text ends up the same way it did the first time.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        feed(&mut ed, "iab<BS><BS>done<Esc>");
        assert_eq!(ed.buffer.text(), "doneone\ntwo\n");
        feed(&mut ed, "j0.");
        assert_eq!(ed.buffer.text(), "doneone\ndonetwo\n");

        // A change that is not a session forgets the last session's text.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        feed(&mut ed, "iXX<Esc>");
        feed(&mut ed, "x");
        feed(&mut ed, ".");
        // The delete ran again rather than the text of the session: `.` repeats
        // the *change* that came last, and `x` came after the typing.
        assert_eq!(ed.buffer.text(), "Xne\n");
    }

    #[test]
    fn dot_keeps_the_change_through_a_yank_and_an_undo() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abcdef\n");
        feed(&mut ed, "x");
        // A yank changes nothing and is not what `.` repeats.
        feed(&mut ed, "yiw");
        feed(&mut ed, ".");
        assert_eq!(ed.buffer.text(), "cdef\n");
        // Undoing is not a change either: the change that is remembered is the
        // one that was made, so `.` puts it back.
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "bcdef\n");
        feed(&mut ed, ".");
        assert_eq!(ed.buffer.text(), "cdef\n", "`.` re-applies the change");
    }

    #[test]
    fn a_register_keeps_what_a_named_yank_took() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        feed(&mut ed, "\"ayyj");
        assert_eq!(
            ed.register.text(),
            "one\n",
            "the unnamed register took it too"
        );
        assert_eq!(
            ed.registers.get('a').map(|r| r.text().to_string()),
            Some("one\n".to_string())
        );
        // And `\"ap` puts *that* register, not the nonsense the unnamed one has
        // picked up since.
        feed(&mut ed, "dd");
        assert_eq!(ed.buffer.text(), "one\n", "the line went");
        assert_eq!(ed.register.text(), "two\n", "and the unnamed one has it");
        feed(&mut ed, "\"ap");
        assert_eq!(
            ed.buffer.text(),
            "one\none\n",
            "register `a` puts its own text"
        );
    }

    #[test]
    fn the_black_hole_swallows_a_delete() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        feed(&mut ed, "yy"); // the unnamed register holds `one`
        feed(&mut ed, "j\"_dd");
        assert_eq!(ed.buffer.text(), "one\n");
        assert_eq!(ed.register.text(), "one\n", "the unnamed one is untouched");
        assert_eq!(
            ed.registers.get('1').map(|r| r.text().to_string()),
            None,
            "and no history entry"
        );
        feed(&mut ed, "p");
        assert_eq!(ed.buffer.text(), "one\none\n", "so `p` still puts the yank");
    }

    #[test]
    fn the_numbered_and_special_registers_remember_for_themselves() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\nthree\n");
        feed(&mut ed, "yyjdd"); // yank `one`, delete `two`
        assert_eq!(
            ed.registers.get('0').map(|r| r.text().to_string()),
            Some("one\n".to_string())
        );
        assert_eq!(
            ed.registers.get('1').map(|r| r.text().to_string()),
            Some("two\n".to_string())
        );
        assert_eq!(ed.register.text(), "two\n", "the unnamed one is the delete");

        // `\"0p` puts the last *yank* back, whatever was deleted after it.
        feed(&mut ed, "\"0p");
        assert_eq!(ed.buffer.text(), "one\nthree\none\n");

        // `\"+y` hands the text to the frontend, which is the half of the
        // clipboard the kernel cannot reach by itself.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("copied\n");
        assert_eq!(ed.clipboard, None);
        feed(&mut ed, "\"+yy");
        assert_eq!(ed.clipboard.as_deref(), Some("copied\n"));
        assert_eq!(
            ed.registers.clipboard().text(),
            "copied\n",
            "and it is kept"
        );

        // A paste from the terminal is what the clipboard holds, so `\"+p` can
        // put back what the terminal just pasted.
        ed.note_clipboard_text("from the system\n");
        assert_eq!(ed.registers.clipboard().text(), "from the system\n");
        feed(&mut ed, "\"+p");
        assert!(
            ed.buffer.text().contains("from the system\n"),
            "{}",
            ed.buffer.text()
        );
    }

    #[test]
    fn a_register_name_that_is_not_one_is_reported() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        feed(&mut ed, "\"!");
        assert_eq!(ed.message, "E354: Invalid register name: !");
        assert_eq!(ed.register_prefix, crate::register::Prefix::None);
        feed(&mut ed, "x");
        assert_eq!(ed.buffer.text(), "ne\n", "and the next key works normally");
    }

    #[test]
    fn reg_lists_what_the_registers_hold() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        feed(&mut ed, "\"ayy");
        ed.execute_cmdline("reg");
        assert!(
            ed.message.starts_with("--- Registers ---"),
            "{}",
            ed.message
        );
        assert!(
            ed.message.contains("\"a   one^J linewise"),
            "{}",
            ed.message
        );
        assert!(ed.message.contains("\"0   one^J"), "{}", ed.message);
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
    fn text_objects_name_the_thing_the_operator_runs_over() {
        // `diw` on the middle word.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("alpha bravo charlie\n");
        feed(&mut ed, "wdiw");
        assert_eq!(
            ed.buffer.text(),
            "alpha  charlie\n",
            "the word, not the space"
        );
        assert_eq!(ed.cursor.col, 6, "and the cursor stays where it started");

        // `daw` takes the space after it, so no double space is left behind.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("alpha bravo charlie\n");
        feed(&mut ed, "wdaw");
        assert_eq!(ed.buffer.text(), "alpha charlie\n");

        // ...and at the end of a line it takes the space *before* it instead.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("alpha bravo\n");
        feed(&mut ed, "wdaw");
        assert_eq!(ed.buffer.text(), "alpha\n");

        // `ciw` changes the word and leaves insert mode waiting.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("alpha bravo\n");
        feed(&mut ed, "wciwtwo<Esc>");
        assert_eq!(ed.buffer.text(), "alpha two\n");

        // `yiw` copies the word without changing anything.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("alpha bravo\n");
        feed(&mut ed, "wyiw$p");
        assert_eq!(
            ed.buffer.text(),
            "alpha bravobravo\n",
            "the word was yanked"
        );
    }

    #[test]
    fn debug_ciw() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("alpha bravo\n");
        feed(&mut ed, "w");
        println!("after w: cursor={:?}", ed.cursor);
        feed(&mut ed, "ciw");
        println!(
            "after ciw: text={:?} cursor={:?} mode={:?}",
            ed.buffer.text(),
            ed.cursor,
            ed.mode
        );
        feed(&mut ed, "two<Esc>");
        println!("after two: text={:?}", ed.buffer.text());
    }

    #[test]
    fn text_objects_for_quotes_and_brackets() {
        // `ci"` on the text between quotes.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("let name = \"old\";\n");
        feed(&mut ed, "ci\"new<Esc>");
        assert_eq!(ed.buffer.text(), "let name = \"new\";\n");

        // `da"` takes the quotes with it.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("let name = \"old\";\n");
        feed(&mut ed, "da\"");
        assert_eq!(ed.buffer.text(), "let name = ;\n");

        // `di(` from anywhere inside the call, including on the opening bracket.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("call(one, two)\n");
        feed(&mut ed, "wdi(");
        assert_eq!(ed.buffer.text(), "call()\n");

        // A block's brackets are a pair across lines.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("fn main() {\n    body();\n}\n");
        feed(&mut ed, "jdi{");
        assert_eq!(
            ed.buffer.text(),
            "fn main() {\n}\n",
            "the body, across lines"
        );

        // And the whole block, brackets included.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("fn main() {\n    body();\n}\n");
        feed(&mut ed, "jda{");
        assert_eq!(ed.buffer.text(), "fn main() \n");

        // `viw` selects it, and `d` takes the selection.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("alpha bravo\n");
        feed(&mut ed, "wviwd");
        assert_eq!(ed.buffer.text(), "alpha \n");
        assert_eq!(ed.mode, Mode::Normal);
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

    #[test]
    fn a_dragged_separator_moves_the_window_beside_it() {
        use inkstone_rpc::{PointerKind, PointerMsg};
        let pointer = |kind: PointerKind, x: u16, y: u16| PointerMsg::new(kind, x, y);

        let mut ed = Editor::for_tests();
        ed.viewport.width = 40;
        ed.viewport.height = 12;
        ed.split_window(Axis::Columns, None);
        let rects = ed.window_rects();
        let (left, right) = (rects[0], rects[1]);
        assert_eq!(right.x, left.width + 1, "a column between them");
        let before = left.width;

        // A press on the separator starts a resize, not a selection.
        ed.handle_pointer(&pointer(PointerKind::Press, right.x - 1, 0));
        assert!(ed.resizing.is_some(), "the separator was picked up");
        assert!(ed.selection.is_none(), "and nothing was selected");
        assert_eq!(ed.mode, Mode::Normal);

        // Dragging it three columns to the left takes three from the left
        // window, and the right one takes them.
        ed.handle_pointer(&pointer(PointerKind::Drag, right.x - 4, 0));
        let rects = ed.window_rects();
        assert_eq!(rects[0].width, before - 3);
        assert_eq!(
            rects[0].width + rects[1].width + 1,
            40,
            "still the whole width"
        );
        // Dragging back is the same distance the other way, from the size the
        // drag started at rather than from the last event.
        ed.handle_pointer(&pointer(PointerKind::Drag, right.x - 1, 0));
        assert_eq!(ed.window_rects()[0].width, before, "back where it started");
        ed.handle_pointer(&pointer(PointerKind::Drag, right.x, 0));
        assert_eq!(
            ed.window_rects()[0].width,
            before + 1,
            "one column on, not two"
        );

        // It cannot be dragged off the screen: a window has to show something.
        ed.handle_pointer(&pointer(PointerKind::Drag, 0, 0));
        assert!(ed.window_rects()[0].width >= 1);
        assert_eq!(
            ed.window_rects()[0].width + ed.window_rects()[1].width + 1,
            40
        );
        ed.handle_pointer(&pointer(PointerKind::Release, 0, 0));
        assert!(ed.resizing.is_none(), "the drag is over");

        // A press on text is still a selection.
        let mut ed = Editor::for_tests();
        ed.viewport.width = 40;
        ed.viewport.height = 12;
        ed.buffer = Buffer::from_text("alpha\nbeta\n");
        ed.split_window(Axis::Columns, None);
        ed.handle_pointer(&pointer(PointerKind::Press, 0, 0));
        assert!(ed.resizing.is_none());
        assert_eq!(ed.mode, Mode::Visual, "text is selected, as it always was");

        // And the row between two stacked windows: the upper one's status row.
        let mut ed = Editor::for_tests();
        ed.viewport.width = 40;
        ed.viewport.height = 12;
        ed.split_window(Axis::Rows, None);
        let rects = ed.window_rects();
        let (upper, lower) = (rects[0], rects[1]);
        assert_eq!(lower.y, upper.y + upper.height, "they tile the space");
        ed.handle_pointer(&pointer(PointerKind::Press, 1, upper.y + upper.height - 1));
        assert!(ed.resizing.is_some(), "the row between them is a separator");
        ed.handle_pointer(&pointer(
            PointerKind::Drag,
            1,
            upper.y + upper.height - 1 + 2,
        ));
        assert_eq!(ed.window_rects()[0].height, upper.height + 2);
        assert_eq!(
            ed.window_rects()[0].height + ed.window_rects()[1].height,
            11
        );
    }

    #[test]
    fn a_split_can_be_resized() {
        let mut ed = Editor::for_tests();
        ed.viewport.width = 80;
        ed.viewport.height = 24;
        ed.split_window(Axis::Rows, None);
        assert_eq!(ed.windows.len(), 2);
        let current = |ed: &Editor| ed.window_rects()[ed.current_window];
        let other = |ed: &Editor| ed.window_rects()[1 - ed.current_window];
        assert_eq!(
            current(&ed).height + other(&ed).height,
            23,
            "the whole height"
        );
        let equal = current(&ed).height;

        // `:resize 5` is five rows of text: the window keeps a row of its own
        // for its status line, so it is six screen rows.
        ed.execute_cmdline("resize 5");
        assert_eq!(current(&ed).height, 6);
        assert_eq!(
            current(&ed).height + other(&ed).height,
            23,
            "still the whole height"
        );
        let (a, b) = (current(&ed), other(&ed));
        let (above, below) = if a.y <= b.y { (a, b) } else { (b, a) };
        assert_eq!(below.y, above.y + above.height, "they tile the space");

        // A bare `:resize` grows by one; `-2` takes two away and `+3` adds three.
        ed.execute_cmdline("resize");
        assert_eq!(current(&ed).height, 7);
        ed.execute_cmdline("resize -2");
        assert_eq!(current(&ed).height, 5);
        ed.execute_cmdline("resize +3");
        assert_eq!(current(&ed).height, 8);

        // `<C-w>=` brings the equal shares back.
        feed(&mut ed, "<C-w>=");
        assert_eq!(current(&ed).height, equal, "as it was");
        ed.execute_cmdline("resize 9");
        assert_eq!(current(&ed).height, 10);
        feed(&mut ed, "<C-w>=");
        assert_eq!(current(&ed).height, equal, "equal again");
        assert_eq!(current(&ed).height + other(&ed).height, 23);

        // Across: `:vertical resize` is the same arithmetic on widths.
        let mut ed = Editor::for_tests();
        ed.viewport.width = 80;
        ed.viewport.height = 24;
        ed.split_window(Axis::Columns, None);
        ed.execute_cmdline("vertical resize 30");
        let rects = ed.window_rects();
        assert_eq!(rects[1].width, 30, "the window that is up");
        assert_eq!(rects[1].x, rects[0].width + 1, "a column between them");
        assert_eq!(
            rects[0].width + rects[1].width + 1,
            80,
            "and the whole width"
        );

        // A window with no division beside it is the whole rectangle: there is
        // nothing to resize and nothing to break.
        let mut ed = Editor::for_tests();
        ed.viewport.width = 80;
        ed.viewport.height = 24;
        ed.execute_cmdline("resize 5");
        assert_eq!(ed.message, "E36: Not enough room");
        assert_eq!(ed.window_rects()[0].height, 23);

        // A size that is not a number says so.
        let mut ed = Editor::for_tests();
        ed.viewport.height = 24;
        ed.split_window(Axis::Rows, None);
        ed.execute_cmdline("resize wide");
        assert_eq!(ed.message, "E475: Invalid argument: wide");
    }

    #[test]
    fn tabs_keep_their_own_windows_and_share_the_documents() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("tabs-one.txt");
        let two = dir.join("tabs-two.txt");
        std::fs::write(&one, "one\ntwo\n").unwrap();
        std::fs::write(&two, "other\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&one).unwrap();
        ed.viewport.width = 40;
        ed.viewport.height = 12;
        ed.execute_cmdline("vs"); // two windows in the first tab
        assert_eq!(ed.windows.len(), 2);

        // `:tab split` puts the same document in a tab of its own, with one
        // window, and the first tab keeps its two.
        ed.execute_cmdline("tab split");
        assert_eq!(ed.tabs.len(), 2);
        assert_eq!(ed.current_tab, 1);
        assert_eq!(ed.windows.len(), 1, "a new tab starts with one window");
        assert_eq!(ed.buffer.text(), "one\ntwo\n", "the same document");
        assert!(ed.status().ends_with("[2/2]"), "{}", ed.status());

        ed.execute_cmdline("tabprevious");
        assert_eq!(ed.current_tab, 0);
        assert_eq!(ed.windows.len(), 2, "the first tab's windows are its own");
        ed.execute_cmdline("tabnext");
        assert_eq!(ed.current_tab, 1);
        assert_eq!(ed.windows.len(), 1);
        ed.execute_cmdline("tabnext");
        assert_eq!(ed.current_tab, 0, "and they wrap around");

        // The documents are everybody's: opened in one tab, reachable in another.
        ed.execute_cmdline(&format!("e {}", two.display()));
        assert_eq!(ed.documents.len(), 2);
        ed.execute_cmdline("tabnext");
        ed.execute_cmdline("b 1");
        assert_eq!(
            ed.buffer.text(),
            "one\ntwo\n",
            "the first document, from tab 2"
        );
        ed.execute_cmdline("ls");
        assert!(ed.message.contains("tabs-two.txt"), "{}", ed.message);

        // `:tabnew` is a tab on an empty file of its own.
        ed.execute_cmdline("tabnew");
        assert_eq!(ed.tabs.len(), 3);
        assert_eq!(ed.buffer.text(), "\n");
        assert_eq!(ed.buffer.path(), None);

        // Closing tabs, and the last one refusing to go.
        ed.execute_cmdline("tabclose");
        assert_eq!(ed.tabs.len(), 2);
        ed.execute_cmdline("tabclose");
        assert_eq!(ed.tabs.len(), 1);
        assert!(!ed.status().contains('['), "{}", ed.status());
        ed.execute_cmdline("tabclose");
        assert_eq!(ed.tabs.len(), 1, "the last tab stays");
        assert_eq!(ed.message, "E784: Cannot close last tab page");
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
    fn paragraph_objects_take_the_lines_around_the_cursor() {
        let text = "one\ntwo\n\nthree\nfour\n\n\nfive\n";

        // `dip` takes the paragraph's lines, and nothing of the blank ones.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(text);
        feed(&mut ed, "dip");
        assert_eq!(ed.buffer.text(), "\nthree\nfour\n\n\nfive\n");
        assert!(ed.register.is_linewise(), "a paragraph is whole lines");
        // And what it took pastes as lines, the way `dd` does.
        feed(&mut ed, "p");
        assert_eq!(ed.buffer.text(), "\none\ntwo\nthree\nfour\n\n\nfive\n");

        // From inside a paragraph, the whole paragraph goes.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(text);
        feed(&mut ed, "jjjdip");
        assert_eq!(ed.buffer.text(), "one\ntwo\n\n\n\nfive\n");

        // `dap` takes the blank lines after it as well.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(text);
        feed(&mut ed, "dap");
        assert_eq!(ed.buffer.text(), "three\nfour\n\n\nfive\n");

        // ...and the ones before it when the paragraph ends the buffer.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(text);
        feed(&mut ed, "Gdap");
        assert_eq!(ed.buffer.text(), "one\ntwo\n\nthree\nfour\n");

        // On a blank line, `ip` is the blank lines themselves: the run of them
        // the cursor is in, which is what clearing the gap between two
        // paragraphs means.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(text);
        feed(&mut ed, "Gkkdip");
        assert_eq!(ed.buffer.text(), "one\ntwo\n\nthree\nfour\nfive\n");

        // `yip` and the visual form.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(text);
        feed(&mut ed, "yip");
        assert_eq!(ed.register.text(), "one\ntwo\n");
        assert_eq!(ed.buffer.text(), text, "a yank changes nothing");
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(text);
        feed(&mut ed, "vipd");
        assert_eq!(ed.buffer.text(), "\nthree\nfour\n\n\nfive\n");
    }

    #[test]
    fn an_unknown_text_object_leaves_the_editor_alone() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("alpha bravo\n");
        feed(&mut ed, "diz");
        assert_eq!(ed.buffer.text(), "alpha bravo\n", "nothing was deleted");
        assert_eq!(ed.message, "z is not a text object");
        assert_eq!(ed.mode, Mode::Normal);
        assert!(ed.operator.is_none(), "and the operator is spent");
    }

    #[test]
    fn separate_insert_sessions_are_separate_steps() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "ione<Esc>");
        feed(&mut ed, "Atwo<Esc>");
        assert_eq!(ed.buffer.text(), "onetwo\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "one\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "\n");
    }

    #[test]
    fn undoing_a_line_deletion_puts_its_terminator_back() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
        feed(&mut ed, "jdd");
        assert_eq!(ed.buffer.text(), "foo\r\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "foo\r\nbar\r\n");

        // `dd` on the only line takes the buffer's last newline with it, and
        // the newline the invariant hands back belongs to the step: undo must
        // not leave a second one behind.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        feed(&mut ed, "dd");
        assert_eq!(ed.buffer.text(), "\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "abc\n", "not \"abc\\n\\n\"");
    }

    #[test]
    fn undo_and_redo_report_when_they_run_out() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "u");
        assert_eq!(ed.message, "Already at oldest change");

        feed(&mut ed, "ix<Esc>");
        feed(&mut ed, "<C-r>");
        assert_eq!(ed.message, "Already at newest change");
    }

    #[test]
    fn an_edit_after_an_undo_drops_the_redo_stack() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        feed(&mut ed, "x");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "abc\n");
        feed(&mut ed, "x");
        feed(&mut ed, "<C-r>");
        assert_eq!(ed.buffer.text(), "bc\n", "redo had nothing to replay");
        assert_eq!(ed.message, "Already at newest change");
    }

    #[test]
    fn the_first_file_takes_the_empty_window_s_place() {
        // `inkstone file` leaves one document behind, not an empty one and a
        // file — the first file a person opens *is* what they are editing.
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("buffers-first.txt");
        std::fs::write(&path, "one\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.execute_cmdline(&format!("e {}", path.display()));
        assert_eq!(ed.documents.len(), 1);
        assert_eq!(ed.buffer.text(), "one\n");

        // But once something has been typed it is a document like any other.
        let mut ed = Editor::for_tests();
        feed(&mut ed, "ikept<Esc>");
        let other = dir.join("buffers-first-other.txt");
        std::fs::write(&other, "two\n").unwrap();
        ed.execute_cmdline(&format!("e! {}", other.display()));
        assert_eq!(ed.documents.len(), 2, "the typed-in window is kept");
    }

    #[test]
    fn a_second_file_arrives_as_a_document_of_its_own() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("buffers-one.txt");
        let two = dir.join("buffers-two.txt");
        std::fs::write(&one, "one\ntwo\nthree\n").unwrap();
        std::fs::write(&two, "other\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&one).unwrap();
        feed(&mut ed, "jj");
        assert_eq!(ed.cursor.row, 2);

        // `:e` on a *different* file keeps the one that was open.
        ed.execute_cmdline(&format!("e {}", two.display()));
        assert_eq!(ed.documents.len(), 2, "the first document is still open");
        assert_eq!(ed.buffer.text(), "other\n");
        assert_eq!(ed.cursor.row, 0, "a new document starts at the top");
        assert_eq!(ed.message, "\"buffers-two.txt\"");

        // Each document keeps its own cursor.
        ed.execute_cmdline("bp");
        assert_eq!(ed.buffer.text(), "one\ntwo\nthree\n");
        assert_eq!(ed.cursor.row, 2, "the cursor came back with its document");
        ed.execute_cmdline("bn");
        assert_eq!(ed.buffer.text(), "other\n");
        assert_eq!(ed.cursor.row, 0);
    }

    #[test]
    fn each_document_keeps_its_own_undo_history() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("buffers-undo-one.txt");
        let two = dir.join("buffers-undo-two.txt");
        std::fs::write(&one, "one\n").unwrap();
        std::fs::write(&two, "other\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&one).unwrap();
        feed(&mut ed, "ix<Esc>"); // one -> xone
                                  // `:e!` because the document that is open has changes; those changes
                                  // stay with it rather than being thrown away.
        ed.execute_cmdline(&format!("e! {}", two.display()));
        feed(&mut ed, "ohello<Esc>");

        // Undoing in the second document must not touch the first, and must not
        // replay the first document's steps onto it: those hold indices into
        // text this document has never held.
        feed(&mut ed, "u");
        assert_eq!(
            ed.buffer.text(),
            "other\n",
            "the second document's own step"
        );
        ed.execute_cmdline("bp");
        assert_eq!(ed.buffer.text(), "xone\n");
        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "one\n", "the first document's own step");
    }

    #[test]
    fn the_window_keeps_its_size_and_the_document_its_place() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("buffers-view-one.rs");
        let two = dir.join("buffers-view-two.rs");
        std::fs::write(&one, "one\ntwo\nthree\n").unwrap();
        std::fs::write(&two, "fn main() {}\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&one).unwrap();
        ed.viewport.width = 80;
        ed.viewport.height = 10;
        feed(&mut ed, "jj");
        assert_eq!(ed.cursor.row, 2);

        // A file that arrives is looked at from the top, and shows that file's
        // colours: the document is new, the window is the same one.
        ed.execute_cmdline(&format!("e {}", two.display()));
        assert_eq!(ed.cursor.row, 0, "a new file starts at the top");
        assert_eq!(ed.viewport.top_line, 0);
        assert_eq!(ed.viewport.height, 10, "the size is the window's");

        // Coming back to a document brings its cursor with it, and leaves the
        // scroll alone: the scroll belongs to the window that is looking.
        ed.viewport.top_line = 5;
        ed.execute_cmdline("bp");
        assert_eq!(ed.cursor.row, 2, "the document's own place");
        assert_eq!(ed.viewport.top_line, 5, "the window's own scroll");
        ed.execute_cmdline("bn");
        assert_eq!(ed.cursor.row, 0, "and back again");
    }

    #[test]
    fn two_windows_on_one_document_keep_their_own_positions() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("buffers-split.txt");
        std::fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&path).unwrap();
        ed.viewport.width = 80;
        ed.viewport.height = 12;
        feed(&mut ed, "jj");

        // :vs makes a second window beside this one, on the same document and
        // at the same place.
        ed.execute_cmdline("vs");
        assert_eq!(ed.windows.len(), 2, "a window beside another");
        assert_eq!(ed.documents.len(), 1, "on the same document");
        assert_eq!(ed.current_window, 1, "and vim's habit: the new one is up");
        assert_eq!(ed.cursor.row, 2, "the new window copies the position");

        // They move apart, and each keeps what it did.
        feed(&mut ed, "j");
        assert_eq!(ed.cursor.row, 3);
        feed(&mut ed, "<C-w>h");
        assert_eq!(ed.current_window, 0);
        assert_eq!(ed.cursor.row, 2, "the first window is where it was");
        ed.viewport.top_line = 2;
        feed(&mut ed, "<C-w>l");
        assert_eq!(ed.current_window, 1);
        assert_eq!(ed.cursor.row, 3);
        assert_ne!(ed.viewport.top_line, 2, "each window has its own view");
        feed(&mut ed, "<C-w>h");
        assert_eq!(ed.viewport.top_line, 2, "and the first kept its own");

        // Both look at one document, so an edit in one is in the other — and
        // the other does not move.
        feed(&mut ed, "<C-w>l");
        feed(&mut ed, "x");
        assert_eq!(ed.buffer.text(), "one\ntwo\nthree\nour\n");
        feed(&mut ed, "<C-w>h");
        assert_eq!(ed.buffer.text(), "one\ntwo\nthree\nour\n");
        assert_eq!(ed.cursor.row, 2, "the other window did not move");

        // A window below the first one, and only it: the two columns are
        // separate trees, so dividing one leaves the other alone.
        assert_eq!(ed.current_window, 0);
        ed.execute_cmdline("sp");
        assert_eq!(ed.windows.len(), 3);
        let rects = ed.window_rects();
        let full = 12 - 1; // the bottom row of the screen is the message row
        assert!(rects[0].height < full, "the left column is divided");
        assert_eq!(rects[1].height, full, "the right column is not");
        assert_eq!(rects[2].x, rects[0].x, "the new window is under the first");
        assert_eq!(rects[2].width, rects[0].width);
        feed(&mut ed, "<C-w>j");
        assert_eq!(ed.current_window, 2);
    }

    #[test]
    fn both_windows_show_the_text_they_are_looking_at() {
        // Two windows on one document, and a file split beside it: the screen
        // has to carry the text of each, not the placeholder of the live one.
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("buffers-pane-one.txt");
        let two = dir.join("buffers-pane-two.txt");
        std::fs::write(&one, "alpha\nbravo\n").unwrap();
        std::fs::write(&two, "one\ntwo\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&one).unwrap();
        ed.viewport.width = 40;
        ed.viewport.height = 8;
        ed.execute_cmdline("sp");
        let composed = ed.compose();
        let row = |y: u16| composed.grid.row_text(y).trim_end().to_string();
        // The window above keeps the row between them for its status line.
        assert_eq!(row(0), "alpha", "the window above has the text");
        assert_eq!(row(1), "bravo");
        assert!(
            row(3).starts_with("-- NORMAL --  buffers-pane-one.txt"),
            "{}",
            row(3)
        );
        assert_eq!(row(4), "alpha", "and so does the one below");
        assert!(
            row(6).starts_with("-- NORMAL --  buffers-pane-one.txt"),
            "{}",
            row(6)
        );

        // A file split beside the current one leaves the current window's
        // document where it was.
        ed.execute_cmdline(&format!("vs {}", two.display()));
        let composed = ed.compose();
        let row = |y: u16| composed.grid.row_text(y).trim_end().to_string();
        assert_eq!(row(0), "alpha", "the window above is untouched");
        // The split went to the window that was up, which was the lower one.
        assert!(row(4).starts_with("alpha"), "{}", row(4));
        assert!(row(4).contains("│one"), "{}", row(4));
        // The second line of each: the left window still has its own document
        // and the new one has the file that was named.
        assert!(
            row(5).trim_start_matches(['│', ' ']).contains("│two"),
            "{}",
            row(5)
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
    fn closing_a_document_closes_its_windows_and_not_the_others() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("buffers-close-one.txt");
        let two = dir.join("buffers-close-two.txt");
        std::fs::write(&one, "one\n").unwrap();
        std::fs::write(&two, "two\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&one).unwrap();
        ed.execute_cmdline(&format!("e! {}", two.display()));
        ed.execute_cmdline("sp"); // two windows on two.txt
        ed.execute_cmdline("bp"); // the left one goes back to one.txt
        assert_eq!(ed.documents.len(), 2);
        assert_eq!(ed.windows.len(), 2);
        assert_eq!(ed.buffer.text(), "one\n");

        ed.execute_cmdline("bd!"); // closes one.txt and its window
        assert_eq!(ed.documents.len(), 1);
        assert_eq!(ed.windows.len(), 1, "the other window stays");
        assert_eq!(ed.buffer.text(), "two\n");
        assert_eq!(ed.current_window, 0);

        // The last document and window going at once leaves an empty one.
        ed.execute_cmdline("bd!");
        assert_eq!(ed.documents.len(), 1);
        assert_eq!(ed.windows.len(), 1);
        assert_eq!(ed.buffer.text(), "\n");
        assert_eq!(ed.buffer.path(), None);
    }

    #[test]
    fn a_modified_document_is_not_left_behind_by_accident() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("buffers-dirty-one.txt");
        let two = dir.join("buffers-dirty-two.txt");
        std::fs::write(&one, "one\n").unwrap();
        std::fs::write(&two, "other\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&one).unwrap();
        ed.execute_cmdline(&format!("e {}", two.display())); // two documents
        ed.execute_cmdline("bp");
        feed(&mut ed, "ix<Esc>");

        // `:e` and `:bd` both refuse; `!` does what it is told.
        ed.execute_cmdline(&format!("e {}", one.display()));
        assert_eq!(
            ed.message,
            "E37: No write since last change (add ! to override)"
        );
        ed.execute_cmdline("bd");
        assert_eq!(
            ed.message,
            "E37: No write since last change (add ! to override)"
        );
        assert_eq!(ed.documents.len(), 2);

        ed.execute_cmdline("bd!");
        assert_eq!(ed.documents.len(), 1, "the document was closed");
        assert_eq!(
            ed.buffer.text(),
            "other\n",
            "and the other one is on screen"
        );
        assert_eq!(ed.message, "\"buffers-dirty-one.txt\" deleted");

        // `:bn` with only one document open says so rather than doing nothing.
        ed.execute_cmdline("bn");
        assert_eq!(ed.message, "E85: There is no other buffer");
    }

    #[test]
    fn closing_the_last_document_leaves_an_empty_one() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("buffers-last.txt");
        std::fs::write(&path, "text\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&path).unwrap();
        ed.execute_cmdline("bd");
        assert_eq!(ed.documents.len(), 1);
        assert_eq!(
            ed.buffer.text(),
            "\n",
            "one empty line, not a closed editor"
        );
        assert_eq!(ed.buffer.path(), None);
        assert_eq!(ed.cursor.row, 0);
        // And it can be written somewhere new, which is what the window is for.
        // (`:w!`, since the file is there and this buffer never read it.)
        feed(&mut ed, "inew<Esc>");
        ed.execute_cmdline(&format!("w! {}", path.display()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
    }

    #[test]
    fn listing_and_naming_documents() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("buffers-list-one.txt");
        let two = dir.join("buffers-list-two.txt");
        std::fs::write(&one, "one\ntwo\n").unwrap();
        std::fs::write(&two, "other\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&one).unwrap();
        feed(&mut ed, "j");
        ed.execute_cmdline(&format!("e {}", two.display()));

        ed.execute_cmdline("ls");
        assert!(
            ed.message.contains("1  h \"buffers-list-one.txt\" line 2"),
            "{}",
            ed.message
        );
        assert!(
            ed.message.contains("2 %a \"buffers-list-two.txt\" line 1"),
            "{}",
            ed.message
        );

        // A number, a full path and the end of one all find their document.
        ed.execute_cmdline("b 1");
        assert_eq!(ed.buffer.text(), "one\ntwo\n");
        ed.execute_cmdline("b 2");
        assert_eq!(ed.buffer.text(), "other\n");
        ed.execute_cmdline(&format!("b {}", one.display()));
        assert_eq!(ed.buffer.text(), "one\ntwo\n");
        ed.execute_cmdline("b two.txt");
        assert_eq!(ed.buffer.text(), "other\n");
        ed.execute_cmdline("b missing.txt");
        assert_eq!(ed.message, "E94: No matching buffer for missing.txt");
        ed.execute_cmdline("b");
        assert_eq!(ed.message, "E471: Argument required");
    }

    #[test]
    fn reopening_the_same_file_reloads_it_rather_than_duplicating_it() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("buffers-same.txt");
        std::fs::write(&path, "one\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&path).unwrap();
        std::fs::write(&path, "changed on disk\n").unwrap();
        // The same file by the same name, and by a path that leads to the same
        // place: both refill the document instead of opening another one.
        ed.execute_cmdline(&format!("e {}", path.display()));
        assert_eq!(ed.documents.len(), 1);
        assert_eq!(ed.buffer.text(), "changed on disk\n");
        let via_dot = dir.join(".").join("buffers-same.txt");
        ed.execute_cmdline(&format!("e {}", via_dot.display()));
        assert_eq!(ed.documents.len(), 1, "the same file, one document");
    }

    #[test]
    fn a_name_that_cannot_be_read_opens_nothing() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let mut ed = Editor::for_tests();
        // A directory is not a file to read, so nothing happens at all.
        ed.execute_cmdline(&format!("e {}", dir.display()));
        assert_eq!(ed.documents.len(), 1, "nothing was opened");
        assert_eq!(ed.buffer.path(), None, "and nothing was named");
        // The wording of the failure is the platform's, so only that there was
        // one is asserted.
        assert!(!ed.message.is_empty(), "the failure was reported");

        // A path that is not there yet is a new file, and as the first file it
        // takes the empty window's place rather than being added beside it.
        let fresh = dir.join("brand-new.txt");
        ed.execute_cmdline(&format!("e {}", fresh.display()));
        assert_eq!(ed.documents.len(), 1);
        assert_eq!(
            ed.buffer.path().and_then(|p| p.file_name()),
            Some(std::ffi::OsStr::new("brand-new.txt"))
        );
        assert_eq!(ed.buffer.text(), "\n", "a new file starts empty");

        // A second one, though, is a document of its own.
        let other = dir.join("brand-new-two.txt");
        std::fs::write(&other, "two\n").unwrap();
        ed.execute_cmdline(&format!("e {}", other.display()));
        assert_eq!(ed.documents.len(), 2);
    }

    #[test]
    fn undoing_back_to_the_written_state_clears_the_marker() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("undo-mark.txt");
        std::fs::write(&path, "one\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&path).unwrap();
        assert!(
            !ed.is_dirty(),
            "a file that has just been read is not changed"
        );

        feed(&mut ed, "ix<Esc>");
        assert!(ed.is_dirty(), "an edit makes it changed");
        ed.execute_cmdline("w");
        assert!(!ed.is_dirty(), "and writing makes it changed no longer");

        feed(&mut ed, "x");
        assert!(ed.is_dirty());
        feed(&mut ed, "u");
        assert!(
            !ed.is_dirty(),
            "undoing back to the written state clears it"
        );
        feed(&mut ed, "u");
        assert!(ed.is_dirty(), "and undoing past it does not");
        feed(&mut ed, "<C-r>");
        assert!(!ed.is_dirty(), "redo takes it back to the written state");
        feed(&mut ed, "<C-r>");
        assert!(ed.is_dirty(), "and on past it");

        // `:q` believes it, which is the point of the whole thing.
        feed(&mut ed, "u");
        assert!(!ed.is_dirty());
        ed.execute_cmdline("q");
        assert!(
            ed.should_quit,
            "a file that is as it was written can be left"
        );
    }

    #[test]
    fn the_saved_state_is_an_identity_not_a_comparison() {
        // Removing a character and typing it back leaves the same text, and it
        // is still an edit: the state it is in is not the state that was
        // written. Comparing the text instead would call this clean and let
        // `:q` walk away from the file the writer meant to keep.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        assert!(!ed.is_dirty());
        feed(&mut ed, "x"); // one -> ne
        feed(&mut ed, "io<Esc>"); // and back to one
        assert_eq!(ed.buffer.text(), "one\n");
        assert!(ed.is_dirty(), "the same text is not the same state");
        feed(&mut ed, "uu");
        assert!(!ed.is_dirty(), "but undoing the steps lands on it");
    }

    #[test]
    fn a_save_keeps_the_history_but_a_reload_forgets_it() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("undo-save.txt");
        std::fs::write(&path, "one\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&path).unwrap();
        feed(&mut ed, "ix<Esc>");
        ed.execute_cmdline(&format!("w {}", path.display()));
        assert!(!ed.is_dirty());

        feed(&mut ed, "u");
        assert_eq!(ed.buffer.text(), "one\n", "writing cleared no history");
        assert!(ed.is_dirty(), "and undoing makes it modified again");

        ed.execute_cmdline(&format!("e! {}", path.display()));
        feed(&mut ed, "u");
        assert_eq!(
            ed.message, "Already at oldest change",
            "a reload starts a fresh history"
        );
    }

    #[test]
    fn undo_walks_all_the_way_back_and_redo_walks_forward_again() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one two\nthree\n");
        let original = ed.buffer.text();

        feed(&mut ed, "iXY<Esc>wddGA!<Esc>odelete me<Esc>xdd");
        let edited = ed.buffer.text();
        assert_ne!(original, edited, "the sequence has to change something");

        while ed.undo() {}
        assert_eq!(ed.buffer.text(), original, "step by step back to the start");
        while ed.redo() {}
        assert_eq!(ed.buffer.text(), edited, "and forward to the end");
    }

    #[test]
    fn a_normal_mode_cursor_on_the_line_end_snaps_back_to_the_last_character() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("ab\ncd\n");
        // Even if something parks the cursor on the terminator...
        ed.cursor = Cursor::new(0, 2);
        feed(&mut ed, "x");
        // ...the next normal-mode key first brings it back onto `b`.
        assert_eq!(ed.buffer.text(), "a\ncd\n");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
    }

    #[test]
    fn open_line_commands_enter_insert_mode() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        ed.cursor = Cursor::new(0, 1);
        feed(&mut ed, "o");
        assert_eq!(ed.mode, Mode::Insert);
        assert_eq!(ed.buffer.text(), "one\n\ntwo\n");
        assert_eq!(ed.cursor, Cursor::new(1, 0));
        // Leaving insert mode steps back onto the line above, as in vim.
        feed(&mut ed, "<Esc>");
        assert_eq!(ed.cursor, Cursor::new(0, 2));
        feed(&mut ed, "O");
        assert_eq!(ed.buffer.text(), "\none\n\ntwo\n");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
        assert_eq!(ed.mode, Mode::Insert);
    }

    #[test]
    fn append_positions_the_cursor_after_the_current_character() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("ab\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "aX<Esc>");
        assert_eq!(ed.buffer.text(), "aXb\n");
        assert_eq!(ed.cursor, Cursor::new(0, 1));
    }

    #[test]
    fn command_line_collects_edits_and_executes_on_enter() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":");
        assert_eq!(ed.mode, Mode::Command);
        feed(&mut ed, "wq");
        assert_eq!(ed.cmdline, "wq");
        feed(&mut ed, "<BS>");
        assert_eq!(ed.cmdline, "w");
        feed(&mut ed, "<Esc>");
        assert_eq!(ed.mode, Mode::Normal);
        assert_eq!(ed.cmdline, "");
    }

    #[test]
    fn writing_without_a_file_name_reports_e32() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":w<CR>");
        assert_eq!(ed.message, "E32: No file name");
        assert_eq!(ed.mode, Mode::Normal);
        assert!(!ed.should_quit);
    }

    #[test]
    fn quitting_a_dirty_buffer_reports_e37_and_stays() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "ix<Esc>");
        assert!(ed.is_dirty());
        feed(&mut ed, ":q<CR>");
        assert!(!ed.should_quit);
        assert!(ed.message.starts_with("E37"), "{}", ed.message);
        feed(&mut ed, ":q!<CR>");
        assert!(ed.should_quit);
    }

    #[test]
    fn unknown_command_line_reports_e492() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":frobnicate<CR>");
        assert_eq!(ed.message, "E492: Not a command: frobnicate");
    }

    #[test]
    fn a_clean_buffer_quits_immediately() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":q<CR>");
        assert_eq!(ed.mode, Mode::Normal, "we are back in normal mode");
        assert!(ed.cmdline.is_empty());
        assert!(ed.should_quit);
    }

    #[test]
    fn unmapped_keys_report_and_clear_pending() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "<Space>");
        assert_eq!(ed.message, "unmapped key: <Space>");
        // `g` starts a multi-key binding and waits for the rest of it.
        feed(&mut ed, "g");
        assert_eq!(ed.pending, "g");
        // A key name the notation does not know is an error rather than a key,
        // so this uses one it does that is not bound to anything.
        feed(&mut ed, "<Space>");
        assert_eq!(ed.pending, "", "the half-typed sequence is dropped");
        assert_eq!(ed.message, "unmapped key: <Space>");
        // `gq` is one of those bindings, so the same two keys now mean
        // something: the rewrap operator, waiting for a motion.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one two\n");
        feed(&mut ed, "gq");
        assert_eq!(ed.pending, "");
        assert_eq!(ed.operator, Some(Operator::Format), "waiting for a motion");
        assert_eq!(ed.message, "");
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
    fn amp_repeats_the_last_substitution_on_this_line() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one one\ntwo one\n");
        ed.execute_cmdline("s/one/1/g");
        assert_eq!(ed.buffer.text(), "1 1\ntwo one\n");
        // `:&` is the same pattern and replacement on the cursor's line, without
        // the `g` that was used last time.
        feed(&mut ed, "j");
        ed.execute_cmdline("&");
        assert_eq!(ed.buffer.text(), "1 1\ntwo 1\n");

        // With nothing substituted yet there is nothing to repeat.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\n");
        ed.execute_cmdline("&");
        assert_eq!(ed.buffer.text(), "one\n");
        assert!(!ed.message.is_empty(), "and it says so: {}", ed.message);
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
    fn a_substitution_can_use_a_group() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("fn one() {}\n");
        feed(&mut ed, ":s/fn ([a-z]+)/fn changed_\\1<Enter>");
        assert_eq!(ed.buffer.text(), "fn changed_one() {}\n");

        feed(&mut ed, ":%s/(changed)_(one)/\\2_\\1/g<Enter>");
        assert_eq!(ed.buffer.text(), "fn one_changed() {}\n");
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

        // A pattern that asks where a match starts inside itself is refused, and
        // named: the engine cannot cut a match in half, and the refusal is
        // already an `E54` — it must not say `E54: E: …`.
        feed(&mut ed, ":s/\\zs/x/<Enter>");
        assert_eq!(
            ed.message,
            "E54: \\zs is not supported: a match here is a stretch of text and cannot be cut in half"
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
        feed(&mut ed, "/\\zs<Enter>");
        assert_eq!(
            ed.message,
            "E54: \\zs is not supported: a match here is a stretch of text and cannot be cut in half"
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
    fn a_message_survives_until_the_next_key() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":q!<CR>");
        assert!(ed.should_quit);
        ed.should_quit = false;
        assert_eq!(ed.message, "", "the quit command does not set a message");
        ed.message = "hello".into();
        feed(&mut ed, "l");
        assert_eq!(ed.message, "");
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
    fn composing_after_editing_shows_the_text() {
        let mut ed = Editor::for_tests();
        ed.handle_resize(40, 4);
        feed(&mut ed, "ihi<Esc>");
        let out = ed.compose();
        assert_eq!(out.grid.row_text(0).trim_end(), "hi");
        let status = out.grid.row_text(2);
        assert!(status.starts_with("-- NORMAL --"), "{status}");
        assert!(status.contains("[No Name][+]"), "{status}");
    }

    #[test]
    fn cursor_is_visible_inside_the_composed_grid() {
        let mut ed = Editor::for_tests();
        ed.handle_resize(40, 4);
        feed(&mut ed, "ihello<Esc>");
        let out = ed.compose();
        assert!(out.cursor_visible);
        assert_eq!(out.cursor, (4, 0));
    }

    #[test]
    fn normal_mode_never_leaves_the_cursor_past_the_last_character() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        feed(&mut ed, "$");
        assert_eq!(ed.cursor, Cursor::new(0, 2));
        feed(&mut ed, "l");
        assert_eq!(
            ed.cursor,
            Cursor::new(0, 2),
            "`l` cannot step onto the newline"
        );
        feed(&mut ed, "x");
        assert_eq!(
            ed.buffer.text(),
            "ab\n",
            "`x` deletes the character under the cursor"
        );
    }

    #[test]
    fn an_empty_line_pins_the_normal_cursor_to_column_zero() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("ab\n\ncd\n");
        ed.cursor = Cursor::new(1, 0);
        feed(&mut ed, "x");
        assert_eq!(
            ed.buffer.text(),
            "ab\ncd\n",
            "`x` on an empty line joins it downward"
        );
        assert_eq!(
            ed.cursor,
            Cursor::new(1, 0),
            "and the cursor stays on that line"
        );
    }

    #[test]
    fn insert_mode_may_sit_one_past_the_last_character() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "A");
        assert_eq!(ed.cursor, Cursor::new(0, 3));
        feed(&mut ed, "X<Esc>");
        assert_eq!(ed.buffer.text(), "abcX\n");
    }

    #[test]
    fn leaving_insert_mode_steps_back_onto_a_real_character() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "A<Esc>");
        assert_eq!(ed.cursor, Cursor::new(0, 2));
        assert_eq!(ed.mode, Mode::Normal);
    }

    #[test]
    fn backspace_at_the_start_of_a_crlf_line_takes_the_whole_terminator() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
        ed.cursor = Cursor::new(1, 0);
        feed(&mut ed, "i<BS><Esc>");
        assert_eq!(ed.buffer.text(), "foobar\r\n", "no stray carriage return");
    }

    #[test]
    fn the_delete_key_deletes_forward_in_insert_mode() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        ed.cursor = Cursor::new(0, 1);
        feed(&mut ed, "i<Del><Esc>");
        assert_eq!(ed.buffer.text(), "ac\n");
    }

    #[test]
    fn arrow_keys_move_the_cursor_in_insert_mode() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "i");
        assert_eq!(ed.mode, Mode::Insert);

        feed(&mut ed, "<Right><Down>");
        assert_eq!(ed.cursor, Cursor::new(1, 1));
        assert_eq!(
            ed.mode,
            Mode::Insert,
            "navigating does not leave insert mode"
        );
        assert_eq!(ed.buffer.text(), "one\ntwo\n", "and it does not edit");

        feed(&mut ed, "<Up><Left><Home><End>");
        assert_eq!(ed.cursor, Cursor::new(0, 2));
        assert_eq!(ed.mode, Mode::Insert);

        // Typing after navigating inserts at the new position.
        feed(&mut ed, "X<Esc>");
        assert_eq!(ed.buffer.text(), "onXe\ntwo\n");
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
    fn a_failed_save_quit_does_not_quit() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("x\n");
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let impossible = dir.join("no-such-dir").join("deep.txt");

        ed.execute_cmdline(&format!("wq {}", impossible.display()));
        assert!(!ed.should_quit, "the session must survive a failed write");
        assert!(ed.message.contains("i/o error"), "{}", ed.message);
    }

    #[test]
    fn append_at_the_end_of_a_crlf_line_inserts_before_the_carriage_return() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "AX<Esc>");
        assert_eq!(ed.buffer.text(), "fooX\r\nbar\r\n");
    }

    #[test]
    fn deleting_a_crlf_line_takes_the_carriage_return_with_it() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "dd");
        assert_eq!(ed.buffer.text(), "bar\r\n");
    }

    #[test]
    fn opening_a_line_below_a_crlf_line_keeps_the_line_endings_intact() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "o<Esc>");
        assert_eq!(ed.buffer.text(), "foo\r\n\r\nbar\r\n");
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

    #[test]
    fn a_zero_sized_terminal_does_not_corrupt_the_viewport() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(&"line\n".repeat(10));
        ed.cursor = Cursor::new(5, 0);
        ed.handle_resize(0, 0);
        assert_eq!(ed.viewport.top_line, 5, "no meaningless scroll offset");
        assert_eq!(ed.viewport.left_col, 0);
        assert!(!ed.compose().cursor_visible, "nothing is visible");

        ed.handle_resize(80, 10);
        let out = ed.compose();
        assert!(out.cursor_visible);
        assert_eq!(out.cursor, (0, 3), "the cursor is back on screen");
    }

    #[test]
    fn a_zero_width_terminal_still_tracks_the_cursor_column() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(&"wide line\n".repeat(10));
        ed.cursor = Cursor::new(5, 2);
        ed.handle_resize(0, 10);
        assert_eq!(
            ed.viewport.top_line, 0,
            "the cursor line is already on screen"
        );
        assert_eq!(ed.viewport.left_col, 2, "no room to scroll into, so pin it");
    }

    #[test]
    fn edit_refuses_to_discard_unsaved_changes() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reload.txt");
        std::fs::write(&path, "on disk\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&path).unwrap();
        feed(&mut ed, "ix<Esc>");
        assert!(ed.is_dirty());

        ed.execute_cmdline(&format!("e {}", path.display()));
        assert!(ed.message.starts_with("E37"), "{}", ed.message);
        assert_eq!(ed.buffer.text(), "xon disk\n", "the edits are still here");

        ed.execute_cmdline(&format!("e! {}", path.display()));
        assert_eq!(ed.buffer.text(), "on disk\n", "`e!` reloads from disk");
        assert!(!ed.is_dirty());
    }

    #[test]
    fn edit_on_a_clean_buffer_reloads() {
        let dir = std::env::temp_dir().join("inkstone-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clean.txt");
        std::fs::write(&path, "one\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&path).unwrap();
        std::fs::write(&path, "two\n").unwrap();
        ed.execute_cmdline(&format!("e {}", path.display()));
        assert_eq!(ed.buffer.text(), "two\n");
    }

    /// Deterministic pseudo-random key sequence: nothing may panic, and the
    /// buffer/viewport invariants must hold after every single key.
    #[test]
    fn random_key_sequences_keep_the_editor_consistent() {
        use inkstone_rpc::{KeyCode, KeyMsg};

        let alphabet: Vec<KeyMsg> = "iIaAoOxddwbgG:u"
            .chars()
            .map(|c| KeyMsg::new(KeyCode::Char(c)))
            .chain([
                KeyMsg::new(KeyCode::Esc),
                KeyMsg::new(KeyCode::Enter),
                KeyMsg::new(KeyCode::Backspace),
                KeyMsg::new(KeyCode::Tab),
                KeyMsg::new(KeyCode::Left),
                KeyMsg::new(KeyCode::Right),
                KeyMsg::new(KeyCode::Up),
                KeyMsg::new(KeyCode::Down),
                KeyMsg::new(KeyCode::Home),
                KeyMsg::new(KeyCode::End),
                KeyMsg::new(KeyCode::Delete),
                KeyMsg::ctrl('w'),
                KeyMsg::ctrl('r'),
            ])
            .collect();

        let mut ed = Editor::for_tests();
        ed.handle_resize(24, 6);
        let mut seed = 0x2545_f491_4f6c_dd1du64;

        for step in 0..4000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let r = seed as usize;

            if r % 19 == 0 {
                ed.handle_resize((r % 40) as u16, (r % 14) as u16);
            }
            let key = alphabet[r % alphabet.len()].clone();
            ed.handle_key(key);
            ed.should_quit = false; // `:q!` must not end the run

            let text = ed.buffer.text();
            assert!(
                text.ends_with('\n'),
                "step {step}: buffer lost its final newline"
            );
            assert!(ed.buffer.line_count() >= 1, "step {step}");
            assert!(
                ed.cursor.row < ed.buffer.line_count(),
                "step {step}: row {} of {}",
                ed.cursor.row,
                ed.buffer.line_count()
            );
            assert!(
                ed.cursor.col <= ed.buffer.line_len(ed.cursor.row),
                "step {step}: col {} past line length {}",
                ed.cursor.col,
                ed.buffer.line_len(ed.cursor.row)
            );
            if ed.mode == Mode::Normal {
                let len = ed.buffer.line_len(ed.cursor.row);
                assert!(
                    ed.cursor.col < len.max(1),
                    "step {step}: normal mode cursor sits past the last character"
                );
            }
            let composed = ed.compose();
            assert_eq!(
                composed.grid.cells.len(),
                composed.grid.width as usize * composed.grid.height as usize
            );
            if composed.cursor_visible {
                assert!(composed.cursor.0 < composed.grid.width.max(1));
                assert!(composed.cursor.1 < composed.grid.height.max(1));
            }
        }
    }
}
