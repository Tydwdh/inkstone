//! The text buffer: a rope-backed document with an on-disk identity.
//!
//! # Invariant
//! The buffer text always ends with `'\n'`. An "empty" buffer therefore holds a
//! single empty line (`"\n"`), which is what the line-oriented model needs to
//! behave like a real editor. Normalization runs after every mutation, so
//! deleting the final newline simply re-adds it.
//!
//! Rope indices are *character* indices (`char`, i.e. Unicode scalar values),
//! never bytes.
//!
//! # Lines
//! A line is *content plus its terminator*, and the terminator is not part of
//! the content: `Buffer::line` strips it and `Buffer::line_terminator`
//! returns it. A file written with CRLF therefore behaves exactly like one
//! written with LF — `$`, `A`, `o` and `dd` all land where a user expects —
//! while `Buffer::text` still returns the original bytes, so saving never
//! rewrites line endings behind the user's back.
//!
//! The set of terminators is the one the underlying rope recognises (LF,
//! CRLF, CR, VT, FF, NEL, LS, PS), so the two models can never disagree about
//! how many lines a buffer has.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ropey::{Rope, RopeSlice};
use unicode_segmentation::UnicodeSegmentation;

/// How far back [`Buffer::prev_grapheme_boundary`] will look for a grapheme
/// boundary. Long enough for any realistic emoji ZWJ cluster.
const GRAPHEME_LOOKBACK: usize = 64;

/// Everything that can go wrong loading or saving a document.
#[derive(Debug, thiserror::Error)]
pub enum BufferError {
    /// The file could not be read or written.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// The file exists but is not UTF-8, so it is not text this editor can
    /// model losslessly.
    #[error("{}: not valid UTF-8", .0.display())]
    NotUtf8(PathBuf),
    /// A write was attempted on a buffer that has never had a path.
    #[error("E32: No file name")]
    NoPath,
    /// The lines of a listing do not spell out something a directory can be.
    #[error("{0}")]
    Listing(String),
    /// A write was attempted on a terminal, which is a program's screen rather
    /// than a document.
    #[error("E: a terminal is a program's screen, not a file")]
    Terminal,
    /// A listing was asked to be written somewhere other than where it lists.
    /// A listing *is* the directory it read: there is nowhere else to put one.
    #[error("E: a listing is written in place, not as {}", .0.display())]
    ListingPath(PathBuf),
    /// The file changed on disk since this buffer read it, so writing would throw
    /// away somebody else's work.
    #[error("E13: File exists (add ! to override): {}", .0.display())]
    Changed(PathBuf),
}

/// An editable document.
#[derive(Debug)]
pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    /// What the file looked like when this buffer last read or wrote it: its
    /// length and its modification time. `None` means there is nothing on disk to
    /// compare with — a scratch buffer, or a path that did not exist.
    stamp: Option<(u64, std::time::SystemTime)>,
    kind: BufferKind,
    /// What to call a document that has no path to be called by.
    title: Option<String>,
}

/// What the text of a buffer *is*.
///
/// A document holds a file, and writing it writes those bytes. A listing holds
/// what a directory holds, one entry to a line, and writing it makes the
/// directory match the text — which is what makes a file manager out of an
/// editor whose only interface is text. A terminal holds what a program has
/// drawn, and it is the one kind whose text nobody edits: the program rewrites
/// it, and a save would have nowhere to put it.
#[derive(Debug)]
pub(crate) enum BufferKind {
    /// The text is the file the path names.
    File,
    /// The path names a directory. The lines are its entries, and `entries` is
    /// what the directory looked like when this buffer last read or wrote it —
    /// the same fact `stamp` is for a file, and the baseline a save is measured
    /// against.
    Listing { entries: Vec<Entry> },
    /// The text is a program's screen: the rows are its lines, and the screen is
    /// the truth they are a view of.
    Terminal {
        screen: Box<crate::terminal::Screen>,
    },
}

/// One entry of a listing, as the directory last looked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The name, without a trailing separator.
    pub name: String,
    /// Whether it is a directory. A listing shows that as a trailing `/`.
    pub is_dir: bool,
}

impl Buffer {
    /// A new, unnamed, empty buffer (one empty line).
    pub fn empty() -> Self {
        Self {
            rope: Rope::from("\n"),
            path: None,
            stamp: None,
            kind: BufferKind::File,
            title: None,
        }
    }

    /// What the file on disk looks like now, if it is there at all.
    fn stamp_of(path: &Path) -> Option<(u64, std::time::SystemTime)> {
        let metadata = std::fs::metadata(path).ok()?;
        Some((metadata.len(), metadata.modified().ok()?))
    }

    /// Load `path`, or create an empty buffer bound to it if it does not exist.
    ///
    /// A directory is not read as bytes — that is not a thing a directory is —
    /// but as the list of what is in it. What a person means by opening a
    /// directory is as clear as what they mean by opening a file, and it is the
    /// same gesture that gets them there.
    ///
    /// Errors when the file exists but cannot be read, or is not UTF-8.
    pub fn from_path(path: &Path) -> Result<Self, BufferError> {
        if path.is_dir() {
            return Self::listing(path);
        }
        if !path.exists() {
            return Ok(Self {
                rope: Rope::from("\n"),
                path: Some(path.to_path_buf()),
                stamp: None,
                kind: BufferKind::File,
                title: None,
            });
        }
        let bytes = std::fs::read(path)?;
        let text =
            String::from_utf8(bytes).map_err(|_| BufferError::NotUtf8(path.to_path_buf()))?;
        let mut buf = Self {
            rope: Rope::from_str(&text),
            path: Some(path.to_path_buf()),
            stamp: Self::stamp_of(path),
            kind: BufferKind::File,
            title: None,
        };
        buf.normalize();
        Ok(buf)
    }

    /// Read `path` as the listing of a directory.
    ///
    /// The path is kept as it was given rather than resolved, because it is also
    /// what the status line shows and what a save hands back to the filesystem:
    /// the directory a person asked for is the one they keep seeing.
    fn listing(path: &Path) -> Result<Self, BufferError> {
        let entries = read_dir_entries(path)?;
        let mut buf = Self {
            rope: Rope::from_str(&listing_text(&entries)),
            path: Some(path.to_path_buf()),
            stamp: Self::stamp_of(path),
            kind: BufferKind::Listing { entries },
            title: None,
        };
        buf.normalize();
        Ok(buf)
    }

    /// Build a buffer from an in-memory string, unbound to any path.
    pub fn from_text(text: &str) -> Self {
        let mut buf = Self {
            rope: Rope::from_str(text),
            path: None,
            stamp: None,
            kind: BufferKind::File,
            title: None,
        };
        buf.normalize();
        buf
    }

    /// Write the buffer to `path`, or to its own path when `path` is `None`.
    ///
    /// On success the buffer is bound to the written path. Whether the buffer is
    /// *modified* is not a fact about the text, so it is not recorded here: it
    /// is whether the undo state the text is in is the one that was written,
    /// which the edit history answers.
    pub fn save(&mut self, path: Option<&Path>) -> Result<PathBuf, BufferError> {
        self.save_with(path, false)
    }

    /// The same, but writing even when the file changed underneath.
    ///
    /// A write is refused when the file on disk is not the one this buffer read:
    /// something else has written it, and the buffer's text is not what a person
    /// would expect to be replacing. `:w!` says they know, and writes anyway.
    pub(crate) fn save_with(
        &mut self,
        path: Option<&Path>,
        force: bool,
    ) -> Result<PathBuf, BufferError> {
        if self.is_terminal() {
            // A terminal is a program's screen. There is nothing to write and
            // nowhere to write it: the program owns it.
            return Err(BufferError::Terminal);
        }
        if let BufferKind::Listing { entries } = &self.kind {
            let entries = entries.clone();
            return self.save_listing(&entries, path, force);
        }
        let target = match path {
            Some(p) => p.to_path_buf(),
            None => self.path.clone().ok_or(BufferError::NoPath)?,
        };
        if !force {
            self.check_stamp(&target)?;
        }
        std::fs::write(&target, self.text())?;
        self.path = Some(target.clone());
        self.stamp = Self::stamp_of(&target);
        Ok(target)
    }

    /// Refuse a write when what is on disk is not what this buffer read.
    ///
    /// Something else has written the file, and the buffer's text is not what a
    /// person would expect to be replacing. `:w!` says they know, and writes
    /// anyway. For a listing the file is a directory, and what "somebody else
    /// wrote it" means is that its entries are not the ones the lines were
    /// measured against.
    fn check_stamp(&self, path: &Path) -> Result<(), BufferError> {
        let on_disk = Self::stamp_of(path);
        if on_disk.is_some() && on_disk != self.stamp {
            return Err(BufferError::Changed(path.to_path_buf()));
        }
        Ok(())
    }

    /// Write a listing out: make the directory match the lines.
    ///
    /// `original` is what the directory held when this buffer last read or wrote
    /// it, which is what tells a rename from a removal and an arrival.
    fn save_listing(
        &mut self,
        original: &[Entry],
        path: Option<&Path>,
        force: bool,
    ) -> Result<PathBuf, BufferError> {
        let dir = self.path.clone().ok_or(BufferError::NoPath)?;
        // A listing is the directory it lists. `:w` is how it is written, so a
        // name of its own is a misunderstanding worth saying out loud rather
        // than a directory quietly written somewhere else.
        if let Some(asked) = path {
            if Some(asked) != self.path.as_deref() {
                return Err(BufferError::ListingPath(asked.to_path_buf()));
            }
        }
        if !force {
            self.check_stamp(&dir)?;
        }
        let wanted = self.parse_listing()?;
        let ops = plan_listing(original, &wanted)?;
        for op in &ops {
            if let Err(e) = run_listing_op(&dir, op) {
                // A write that stopped half way leaves a directory no listing
                // describes. The baseline follows the filesystem rather than the
                // plan, so that a second `:w` measures against what is really
                // there — and finishes the job instead of redoing it.
                self.kind = BufferKind::Listing {
                    entries: read_dir_entries(&dir).unwrap_or_default(),
                };
                return Err(e);
            }
        }
        self.kind = BufferKind::Listing { entries: wanted };
        self.stamp = Self::stamp_of(&dir);
        Ok(dir)
    }

    /// The entries the lines of this listing name, or the first thing wrong with
    /// them.
    ///
    /// A blank line names nothing, so it is skipped: a listing of an empty
    /// directory holds one — the line model has no other way to say "nothing" —
    /// and so does a line somebody is still typing. The `/` is the listing's own
    /// marker for a directory, the same on every platform, and the platform's
    /// separator is taken as one too, for a person who reaches for the key their
    /// own file manager uses.
    fn parse_listing(&self) -> Result<Vec<Entry>, BufferError> {
        let mut entries: Vec<Entry> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        for row in 0..self.line_count() {
            let line = self.line_string(row);
            if line.is_empty() {
                continue;
            }
            let (name, is_dir) = split_marker(&line);
            let entry = Entry {
                name: name.to_string(),
                is_dir,
            };
            if entry.name.is_empty() {
                return Err(BufferError::Listing(format!(
                    "line {}: a name cannot be empty",
                    row + 1
                )));
            }
            if entry.name == "." || entry.name == ".." {
                return Err(BufferError::Listing(format!(
                    "line {}: `.` and `..` are the directory itself",
                    row + 1
                )));
            }
            if entry.name.contains(['/', std::path::MAIN_SEPARATOR]) {
                return Err(BufferError::Listing(format!(
                    "line {}: a name is one name, not a path",
                    row + 1
                )));
            }
            if seen.insert(entry.name.clone(), row).is_some() {
                return Err(BufferError::Listing(format!(
                    "line {}: two lines name `{}`",
                    row + 1,
                    entry.name
                )));
            }
            entries.push(entry);
        }
        Ok(entries)
    }

    // -- accessors ----------------------------------------------------------

    /// Where this buffer came from, if anywhere. `None` means a scratch buffer,
    /// and a listing's path is the directory it lists.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Name a scratch buffer that is *about* something — a command's output.
    ///
    /// A document with no path has nothing to be called, and `[No Name]` in a
    /// status line says where a window came from not at all. This is the one
    /// thing such a document can say about itself, and it is said by whoever
    /// made it, because only they know.
    pub(crate) fn set_title(&mut self, title: &str) {
        self.title = Some(title.to_string());
    }

    /// A buffer showing what a program has drawn.
    ///
    /// There is no path: a terminal is not a file, and the *screen* is the
    /// truth — the text is a view of it, refreshed whenever the program writes.
    pub(crate) fn from_terminal(program: &str, screen: crate::terminal::Screen) -> Self {
        let mut buf = Self {
            rope: Rope::from_str(&screen.text()),
            path: None,
            stamp: None,
            kind: BufferKind::Terminal {
                screen: Box::new(screen),
            },
            title: Some(format!("!{program}")),
        };
        buf.normalize();
        buf
    }

    /// The screen, when this buffer is a terminal's.
    pub(crate) fn terminal(&self) -> Option<&crate::terminal::Screen> {
        match &self.kind {
            BufferKind::Terminal { screen } => Some(screen),
            BufferKind::File | BufferKind::Listing { .. } => None,
        }
    }

    /// Whether this buffer is a program's screen.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self.kind, BufferKind::Terminal { .. })
    }

    /// Let the program draw: feed it bytes, and take the screen as the new text.
    ///
    /// The text is *replaced*, not edited, and that is the one place a change
    /// does not go through the undo history: a program redrawing its screen is
    /// not an edit somebody made, and a `u` that scrolled a terminal back would
    /// be a `u` that undid the program's work rather than a person's.
    pub(crate) fn terminal_feed(&mut self, bytes: &[u8]) {
        let BufferKind::Terminal { screen } = &mut self.kind else {
            return;
        };
        screen.feed(bytes);
        self.rope = Rope::from_str(&screen.text());
    }

    /// What the program asked to be told, if anything: the answer to "where is
    /// the cursor".
    pub(crate) fn terminal_reply(&mut self) -> Option<Vec<u8>> {
        match &mut self.kind {
            BufferKind::Terminal { screen } => screen.take_reply(),
            BufferKind::File | BufferKind::Listing { .. } => None,
        }
    }

    /// Whether this buffer lists a directory rather than holding a file.
    ///
    /// The one question a command has to ask before treating the text as a file
    /// it could write: `:w` of a listing is not a write.
    pub(crate) fn is_listing(&self) -> bool {
        matches!(self.kind, BufferKind::Listing { .. })
    }

    /// The name to show in the status line.
    pub fn display_name(&self) -> String {
        if self.is_listing() {
            // The name of a directory says very little about where a person is
            // — `src` is four characters and so is `tmp` — so a listing shows
            // the path it was opened with, and the trailing separator is what
            // says it is a directory at all.
            return match self.path.as_deref() {
                // The separator is the platform's, because this is a path in a
                // status line and mixes with the rest of one; the *listing* uses
                // `/`, because that is a format rather than a path.
                Some(path) => format!("{}{}", path.display(), std::path::MAIN_SEPARATOR),
                None => "[No Name]".to_string(),
            };
        }
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| self.title.clone())
            .unwrap_or_else(|| "[No Name]".to_string())
    }

    /// The whole document as it would be written to disk.
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// The text in the character range `[start, end)`, clamped to the buffer.
    pub(crate) fn slice_text(&self, start: usize, end: usize) -> String {
        let len = self.len_chars();
        let start = start.min(len);
        let end = end.clamp(start, len);
        self.rope.slice(start..end).to_string()
    }

    /// Length in characters, terminator included.
    pub(crate) fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Number of lines; an empty buffer has one.
    pub fn line_count(&self) -> usize {
        // ropey counts the phantom line after a trailing newline; the invariant
        // guarantees a trailing newline, so there is always at least one line.
        self.rope.len_lines() - 1
    }

    /// The character at `idx`, or `None` past the end.
    pub(crate) fn char_at(&self, idx: usize) -> Option<char> {
        self.rope.get_char(idx)
    }

    /// Line `i` without its terminator. Panics if `i` is out of range.
    pub fn line(&self, i: usize) -> RopeSlice<'_> {
        strip_terminator(self.rope.line(i))
    }

    /// The terminator of line `i`: `"\n"`, `"\r\n"`, `"\r"`, ... or `""`.
    pub(crate) fn line_terminator(&self, i: usize) -> String {
        let full = self.rope.line(i);
        full.slice(self.line(i).len_chars()..).to_string()
    }

    /// How many characters line `i`'s terminator takes.
    pub(crate) fn line_terminator_len(&self, i: usize) -> usize {
        self.rope.line(i).len_chars() - self.line_len(i)
    }

    /// Line `i` as an owned string, for callers that need to keep it.
    pub fn line_string(&self, i: usize) -> String {
        self.line(i).to_string()
    }

    /// Number of characters in line `i`, excluding the newline.
    pub(crate) fn line_len(&self, i: usize) -> usize {
        self.line(i).len_chars()
    }

    /// Turn a character index into a `(line, column)` position.
    ///
    /// A position inside a terminator reports as the end of its line, so a
    /// caller can never get a column past [`Buffer::line_len`].
    pub(crate) fn char_to_line_col(&self, char_idx: usize) -> (usize, usize) {
        let len = self.len_chars();
        if len == 0 {
            return (0, 0);
        }
        let idx = char_idx.min(len - 1);
        // The final newline terminates the last line rather than starting a new one.
        if idx + 1 == len {
            let line = self.line_count() - 1;
            return (line, self.line_len(line));
        }
        let line = self.rope.char_to_line(idx);
        let start = self.rope.line_to_char(line);
        (line, idx - start)
    }

    /// Turn a `(line, column)` position into a character index, clamping both.
    ///
    /// A column of `line_len(line)` is the position of that line's terminator.
    pub fn line_col_to_char(&self, line: usize, col: usize) -> usize {
        if line >= self.line_count() {
            return self.len_chars().saturating_sub(1);
        }
        let start = self.rope.line_to_char(line);
        start + col.min(self.line_len(line))
    }

    // -- mutation -----------------------------------------------------------

    /// The characters in `[start, end)`, as a string. An end beyond the end of
    /// the buffer is clamped, the way [`Buffer::replace_range`] clamps.
    pub(crate) fn text_range(&self, start: usize, end: usize) -> String {
        let len = self.len_chars();
        let start = start.min(len);
        let end = end.clamp(start, len);
        self.rope.slice(start..end).chunks().collect()
    }

    /// Replace the characters in `[start, end)` with `text`.
    ///
    /// This is the only way text changes, and the trailing-newline invariant is
    /// restored once, after both halves have happened. That matters to undo: a
    /// step that takes the last terminator is put back by a removal followed by
    /// an insertion, and a buffer that repaired its newline in between would
    /// come back with one newline too many.
    pub(crate) fn replace_range(&mut self, start: usize, end: usize, text: &str) {
        let len = self.len_chars();
        let start = start.min(len);
        let end = end.clamp(start, len);
        if start == end && text.is_empty() {
            return;
        }
        self.rope.remove(start..end);
        self.rope.insert(start, text);
        self.normalize();
    }

    /// The character index of the grapheme boundary strictly after the grapheme
    /// that starts at `char_idx`.
    ///
    /// Used where a cursor sits *before* something — `p` puts its text after the
    /// character under the cursor, and a wide grapheme is one character with two
    /// cells in it.
    pub(crate) fn next_grapheme_boundary(&self, char_idx: usize) -> usize {
        let len = self.len_chars();
        let idx = char_idx.min(len);
        if idx >= len {
            return len;
        }
        let end = (idx + GRAPHEME_LOOKBACK).min(len);
        let window: String = self.rope.slice(idx..end).to_string();
        match window.graphemes(true).next() {
            Some(grapheme) => idx + grapheme.chars().count(),
            None => idx,
        }
    }

    /// The character index of the grapheme boundary strictly before `char_idx`.
    pub(crate) fn prev_grapheme_boundary(&self, char_idx: usize) -> usize {
        let idx = char_idx.min(self.len_chars());
        if idx == 0 {
            return 0;
        }
        let start = idx.saturating_sub(GRAPHEME_LOOKBACK);
        let window: String = self.rope.slice(start..idx).to_string();
        let window_len = window.chars().count();
        // Walk the window's graphemes, tracking their start offsets in *characters*.
        let mut best = 0;
        let mut off = 0;
        for g in window.graphemes(true) {
            if off >= window_len {
                break;
            }
            best = off;
            off += g.chars().count();
        }
        start + best
    }

    /// Restore the trailing-newline invariant.
    fn normalize(&mut self) {
        let len = self.rope.len_chars();
        if len == 0 || self.rope.char(len - 1) != '\n' {
            self.rope.insert_char(len, '\n');
        }
    }
}

/// The name a listing line writes, and whether it is marked a directory.
///
/// A directory is marked with a `/` — the listing's own format, the same on
/// every platform so that a listing written on one is a listing on another —
/// and with the platform's separator as well, for a person who reaches for the
/// key their own file manager uses.
fn split_marker(line: &str) -> (&str, bool) {
    if let Some(name) = line.strip_suffix('/') {
        return (name, true);
    }
    if std::path::MAIN_SEPARATOR != '/' {
        if let Some(name) = line.strip_suffix(std::path::MAIN_SEPARATOR) {
            return (name, true);
        }
    }
    (line, false)
}

/// What a save has to do to a directory to make it match the lines.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ListingOp {
    /// Make an entry, empty: a file, or a directory when `is_dir`.
    Create(Entry),
    /// Take an entry out. A directory goes only when it is empty, which
    /// `fs::remove_dir` is what enforcing that means.
    Remove(Entry),
    /// Put an entry under another name.
    Rename { from: String, to: String },
}

/// What has to happen for `dir` to hold `wanted` instead of `original`.
///
/// The name is the identity, so a line that keeps its name is nothing to do —
/// which is what makes reordering a listing an edit with no effect rather than a
/// storm of renames. One name gone and one arrived is a rename, because that is
/// the only way a rename can be meant: anything more is somebody who removed
/// some entries and added others, and pairing them up would be guessing with
/// their files. (A name kept with its marker changed contradicts itself, and
/// says so.)
fn plan_listing(original: &[Entry], wanted: &[Entry]) -> Result<Vec<ListingOp>, BufferError> {
    for entry in wanted {
        if let Some(before) = original.iter().find(|one| one.name == entry.name) {
            if before.is_dir != entry.is_dir {
                let kind = if before.is_dir { "directory" } else { "file" };
                let asked = if entry.is_dir { "directory" } else { "file" };
                return Err(BufferError::Listing(format!(
                    "`{}` is a {kind}, and one line cannot make it a {asked}",
                    entry.name
                )));
            }
        }
    }
    let removed: Vec<Entry> = original
        .iter()
        .filter(|before| !wanted.iter().any(|one| one.name == before.name))
        .cloned()
        .collect();
    let added: Vec<Entry> = wanted
        .iter()
        .filter(|one| !original.iter().any(|before| before.name == one.name))
        .cloned()
        .collect();
    let renamed = match (&removed[..], &added[..]) {
        ([before], [after]) if before.is_dir == after.is_dir => Some(ListingOp::Rename {
            from: before.name.clone(),
            to: after.name.clone(),
        }),
        _ => None,
    };
    if let Some(op) = renamed {
        return Ok(vec![op]);
    }
    let mut ops: Vec<ListingOp> = Vec::new();
    // Removals go first so that a name is free by the time it is wanted: a
    // listing may hold names the filesystem must not have two of at once.
    ops.extend(removed.into_iter().map(ListingOp::Remove));
    ops.extend(added.into_iter().map(ListingOp::Create));
    Ok(ops)
}

/// Do one thing a listing asked for.
///
/// A file is created with `create_new`, so that a listing can never truncate
/// one: an entry the buffer thinks is new but the filesystem does not is a
/// disagreement to report, not a file to empty.
fn run_listing_op(dir: &Path, op: &ListingOp) -> Result<(), BufferError> {
    let attempt = || -> std::io::Result<()> {
        match op {
            ListingOp::Create(entry) => {
                let target = dir.join(&entry.name);
                if entry.is_dir {
                    std::fs::create_dir(target)
                } else {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(target)
                        .map(|_| ())
                }
            }
            ListingOp::Remove(entry) => {
                let target = dir.join(&entry.name);
                if entry.is_dir {
                    std::fs::remove_dir(target)
                } else {
                    std::fs::remove_file(target)
                }
            }
            ListingOp::Rename { from, to } => std::fs::rename(dir.join(from), dir.join(to)),
        }
    };
    attempt().map_err(|e| BufferError::Listing(format!("{}: {}", named(op), e)))
}

/// The entry an operation is about, for a message that says which one went
/// wrong.
fn named(op: &ListingOp) -> String {
    match op {
        ListingOp::Create(entry) | ListingOp::Remove(entry) => entry.name.clone(),
        ListingOp::Rename { to, .. } => to.clone(),
    }
}

/// The entries of `dir`, directories first and each group by name.
///
/// Directories first because that is what a listing is for: walking down is what
/// a person does with one, and the names they walk into should not be scattered
/// through the files. The order within a group is the one `:vimgrep` walks a
/// directory in, so the two agree about what "in order" means.
fn read_dir_entries(dir: &Path) -> Result<Vec<Entry>, BufferError> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false),
        })
        .collect();
    entries.sort_by(|one, other| {
        other
            .is_dir
            .cmp(&one.is_dir)
            .then_with(|| one.name.cmp(&other.name))
    });
    Ok(entries)
}

/// The text a listing of `entries` shows: one name per line, a directory marked
/// with a trailing `/`.
///
/// The marker is the whole of what a listing says about its entries, and it is
/// what a person can type to say "make this a directory" — the format is the
/// interface.
fn listing_text(entries: &[Entry]) -> String {
    let mut text = String::new();
    for entry in entries {
        text.push_str(&entry.name);
        if entry.is_dir {
            text.push('/');
        }
        text.push('\n');
    }
    text
}

/// Remove one terminator from the end of a rope line.
///
/// Mirrors the rope's own idea of a line break, so a line never ends up with
/// half a terminator attached to its content.
fn strip_terminator(line: RopeSlice<'_>) -> RopeSlice<'_> {
    let n = line.len_chars();
    if n == 0 {
        return line;
    }
    match line.char(n - 1) {
        // A CRLF pair is a single terminator.
        '\n' if n >= 2 && line.char(n - 2) == '\r' => line.slice(..n - 2),
        '\n' | '\r' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}' => {
            line.slice(..n - 1)
        }
        _ => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_has_one_empty_line() {
        let buf = Buffer::empty();
        assert_eq!(buf.text(), "\n");
        assert_eq!(buf.line_count(), 1);
        assert_eq!(buf.line_string(0), "");
        assert_eq!(buf.len_chars(), 1);
    }

    #[test]
    fn trailing_newline_invariant_holds_after_edits() {
        let mut buf = Buffer::empty();
        buf.replace_range(0, 0, "hello");
        assert_eq!(buf.text(), "hello\n");
        buf.replace_range(5, 6, ""); // delete the newline itself
        assert_eq!(buf.text(), "hello\n", "final newline is restored");
    }

    #[test]
    fn deleting_a_mid_file_newline_joins_lines() {
        let mut buf = Buffer::from_text("alpha\nbeta\n");
        assert_eq!(buf.line_count(), 2);
        buf.replace_range(5, 6, ""); // the newline after "alpha"
        assert_eq!(buf.text(), "alphabeta\n");
        assert_eq!(buf.line_count(), 1);
        assert_eq!(buf.line_string(0), "alphabeta");
    }

    #[test]
    fn removing_everything_leaves_one_empty_line() {
        let mut buf = Buffer::from_text("abc\ndef\n");
        buf.replace_range(0, buf.len_chars(), "");
        assert_eq!(buf.text(), "\n");
        assert_eq!(buf.line_count(), 1);
    }

    #[test]
    fn line_and_char_index_round_trip() {
        let buf = Buffer::from_text("ab\ncdef\n\ng\n");
        assert_eq!(buf.line_count(), 4);
        assert_eq!(buf.line_string(0), "ab");
        assert_eq!(buf.line_string(1), "cdef");
        assert_eq!(buf.line_string(2), "");
        assert_eq!(buf.line_string(3), "g");

        assert_eq!(buf.line_col_to_char(0, 0), 0);
        assert_eq!(buf.line_col_to_char(0, 2), 2);
        assert_eq!(buf.line_col_to_char(1, 0), 3);
        assert_eq!(buf.line_col_to_char(1, 4), 7);
        assert_eq!(buf.line_col_to_char(3, 1), 10);
        // Column past the end clamps to the end of the line.
        assert_eq!(buf.line_col_to_char(0, 99), 2);

        for (idx, expect) in [(0, (0, 0)), (4, (1, 1)), (8, (2, 0)), (10, (3, 1))] {
            assert_eq!(buf.char_to_line_col(idx), expect, "idx {idx}");
        }
    }

    #[test]
    fn newline_chars_map_to_end_of_line() {
        let buf = Buffer::from_text("ab\ncd\n");
        assert_eq!(buf.char_to_line_col(2), (0, 2));
        assert_eq!(buf.char_to_line_col(5), (1, 2));
    }

    #[test]
    fn prev_grapheme_boundary_handles_multibyte_and_clusters() {
        // "é" as a combining sequence: 'e' + U+0301
        let buf = Buffer::from_text("e\u{301}x\n");
        assert_eq!(buf.len_chars(), 4);
        // Boundary before the cluster's second char collapses to the cluster start.
        assert_eq!(buf.prev_grapheme_boundary(1), 0);
        assert_eq!(buf.prev_grapheme_boundary(2), 0);
        assert_eq!(buf.prev_grapheme_boundary(3), 2);
        assert_eq!(buf.prev_grapheme_boundary(0), 0);

        // Family emoji: 4 codepoints joined by ZWJ.
        let family = Buffer::from_text("👨\u{200d}👩\u{200d}👧\u{200d}👦!\n");
        assert_eq!(family.prev_grapheme_boundary(7), 0);
        assert_eq!(family.prev_grapheme_boundary(8), 7);

        // CJK is a single grapheme of its own.
        let cjk = Buffer::from_text("你好\n");
        assert_eq!(cjk.prev_grapheme_boundary(1), 0);
        assert_eq!(cjk.prev_grapheme_boundary(2), 1);
    }

    #[test]
    fn next_grapheme_boundary_steps_over_whole_clusters() {
        let buf = Buffer::from_text("e\u{301}x\n");
        assert_eq!(
            buf.next_grapheme_boundary(0),
            2,
            "the combining pair is one"
        );
        assert_eq!(buf.next_grapheme_boundary(2), 3);
        assert_eq!(
            buf.next_grapheme_boundary(3),
            4,
            "the terminator is one too"
        );
        assert_eq!(
            buf.next_grapheme_boundary(4),
            4,
            "and the end stays the end"
        );

        let cjk = Buffer::from_text("你好\n");
        assert_eq!(cjk.next_grapheme_boundary(0), 1, "one character each");
        assert_eq!(cjk.next_grapheme_boundary(1), 2);
    }

    #[test]
    fn from_path_round_trips_and_binds_the_path() {
        let dir = std::env::temp_dir().join("inkstone-buffer-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("round-trip.txt");
        let _ = std::fs::remove_file(&path);

        // Missing file: empty buffer bound to the path.
        let mut buf = Buffer::from_path(&path).unwrap();
        assert_eq!(buf.text(), "\n");
        assert_eq!(buf.path(), Some(path.as_path()));
        assert_eq!(buf.display_name(), "round-trip.txt");

        buf.replace_range(0, 0, "line one");
        let written = buf.save(None).unwrap();
        assert_eq!(written, path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "line one\n");

        let reloaded = Buffer::from_path(&path).unwrap();
        assert_eq!(reloaded.text(), "line one\n");

        // Existing file without a trailing newline gets normalized on load.
        std::fs::write(&path, "no newline").unwrap();
        let normalized = Buffer::from_path(&path).unwrap();
        assert_eq!(normalized.text(), "no newline\n");
    }

    #[test]
    fn save_without_a_path_is_an_error() {
        let mut buf = Buffer::empty();
        let err = buf.save(None).unwrap_err();
        assert_eq!(err.to_string(), "E32: No file name");
    }

    #[test]
    fn save_to_an_explicit_path_binds_the_buffer() {
        let dir = std::env::temp_dir().join("inkstone-buffer-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("explicit.txt");
        let _ = std::fs::remove_file(&path);

        let mut buf = Buffer::empty();
        buf.replace_range(0, 0, "written");
        buf.save(Some(&path)).unwrap();
        assert_eq!(buf.path(), Some(path.as_path()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "written\n");
    }

    #[test]
    fn non_utf8_files_are_rejected() {
        let dir = std::env::temp_dir().join("inkstone-buffer-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("binary.bin");
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();

        let err = Buffer::from_path(&path).unwrap_err();
        assert!(matches!(err, BufferError::NotUtf8(_)), "got {err:?}");
    }

    #[test]
    fn missing_parent_directory_is_an_io_error() {
        let dir = std::env::temp_dir().join("inkstone-buffer-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("no-such-dir").join("deep.txt");
        let mut buf = Buffer::from_text("x\n");
        buf.replace_range(0, 0, "dirty");
        let err = buf.save(Some(&path)).unwrap_err();
        assert!(matches!(err, BufferError::Io(_)), "got {err:?}");
    }

    #[test]
    fn crlf_line_endings_stay_out_of_the_line_model() {
        let buf = Buffer::from_text("a\r\nbb\r\n");
        // The bytes are preserved exactly...
        assert_eq!(buf.text(), "a\r\nbb\r\n");
        // ...but the carriage return is part of the terminator, not of the line.
        assert_eq!(buf.line_count(), 2);
        assert_eq!(buf.line_string(0), "a");
        assert_eq!(buf.line_len(0), 1);
        assert_eq!(buf.line_string(1), "bb");
        assert_eq!(buf.line_len(1), 2);
        assert_eq!(buf.line_terminator(0), "\r\n");
        assert_eq!(buf.line_terminator_len(0), 2);

        // A position on the carriage return reports as the line end.
        assert_eq!(buf.char_to_line_col(1), (0, 1));
        assert_eq!(buf.char_to_line_col(4), (1, 1));
        assert_eq!(buf.char_to_line_col(6), (1, 2));
        // The end of a line is the start of its terminator.
        assert_eq!(buf.line_col_to_char(0, 1), 1);
        assert_eq!(buf.line_col_to_char(1, 2), 5);
    }

    #[test]
    fn a_lone_carriage_return_counts_as_a_line_break() {
        // Whatever the rope calls a break, the line model calls a break too.
        let buf = Buffer::from_text("a\rb\n");
        assert_eq!(buf.line_count(), 2);
        assert_eq!(buf.line_string(0), "a");
        assert_eq!(buf.line_terminator(0), "\r");
        assert_eq!(buf.line_string(1), "b");
    }

    #[test]
    fn editing_never_rewrites_the_bytes_on_disk() {
        let mut buf = Buffer::from_text("a\r\nb\r\n");
        let after_a = buf.line_col_to_char(0, 1);
        buf.replace_range(after_a, after_a, "X");
        assert_eq!(buf.text(), "aX\r\nb\r\n");
        let (start, end) = (0, buf.line_len(0) + buf.line_terminator_len(0));
        buf.replace_range(start, end, "");
        assert_eq!(buf.text(), "b\r\n");
    }

    /// A directory of the shape every listing test wants: two directories and
    /// two files, one of each named so that byte order and eye order can be told
    /// apart (`Cargo.toml` before `README.md`, `docs` before `src`).
    fn a_directory(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(dir.join("README.md"), "# hi\n").unwrap();
        dir
    }

    #[test]
    fn a_directory_reads_as_a_listing_with_directories_first() {
        let dir = a_directory("inkstone-buffer-listing");
        let buf = Buffer::from_path(&dir).unwrap();

        assert!(buf.is_listing());
        assert_eq!(buf.text(), "docs/\nsrc/\nCargo.toml\nREADME.md\n");
        assert_eq!(buf.path(), Some(dir.as_path()));
        // A listing shows the path it was opened with, and says it is one.
        assert_eq!(
            buf.display_name(),
            format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR)
        );
    }

    #[test]
    fn a_relative_directory_stays_relative_in_the_display_name() {
        // Nothing is resolved: the path a person typed is the path they keep
        // seeing, in the status line and in the messages that name it.
        let buf = Buffer::from_path(Path::new(".")).unwrap();
        assert!(buf.is_listing());
        assert_eq!(
            buf.display_name(),
            format!(".{}", std::path::MAIN_SEPARATOR)
        );
    }

    #[test]
    fn an_empty_directory_is_a_listing_of_one_empty_line() {
        let dir = std::env::temp_dir().join("inkstone-buffer-listing-empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let buf = Buffer::from_path(&dir).unwrap();
        assert_eq!(buf.text(), "\n", "the line model holds even for nothing");
        assert_eq!(buf.line_count(), 1);
    }

    #[test]
    fn a_file_that_is_not_there_is_still_a_file() {
        // The directory branch must not swallow the empty-document branch.
        let dir = std::env::temp_dir().join("inkstone-buffer-listing-missing");
        std::fs::create_dir_all(&dir).unwrap();
        let buf = Buffer::from_path(&dir.join("nothing.txt")).unwrap();
        assert!(!buf.is_listing());
        assert_eq!(buf.text(), "\n");
    }

    /// Replace a listing's lines with what a person would have typed.
    fn listing_of(dir: &Path) -> Buffer {
        Buffer::from_path(dir).unwrap()
    }

    fn retype(buf: &mut Buffer, text: &str) {
        let end = buf.len_chars();
        buf.replace_range(0, end, text);
    }

    #[test]
    fn a_listing_writes_the_entries_its_lines_name() {
        let dir = a_directory("inkstone-buffer-listing-write");
        let mut buf = listing_of(&dir);
        retype(
            &mut buf,
            "docs/\nsrc/\nCargo.toml\nREADME.md\napp.rs\ntools/\n",
        );

        buf.save(None).unwrap();

        assert!(dir.join("app.rs").is_file());
        assert_eq!(std::fs::metadata(dir.join("app.rs")).unwrap().len(), 0);
        assert!(dir.join("tools").is_dir());
        assert_eq!(
            buf.text(),
            "docs/\nsrc/\nCargo.toml\nREADME.md\napp.rs\ntools/\n"
        );
        // And the old entries are still there: a listing writes what it says.
        assert!(dir.join("docs").is_dir());
    }

    #[test]
    fn one_name_out_and_one_name_in_is_a_rename() {
        let dir = a_directory("inkstone-buffer-listing-rename");
        let mut buf = listing_of(&dir);
        retype(&mut buf, "docs/\nsrc/\nCargo.toml\nREADME.txt\n");

        buf.save(None).unwrap();

        assert!(!dir.join("README.md").exists());
        assert!(dir.join("README.txt").is_file());
        assert_eq!(
            std::fs::read_to_string(dir.join("README.txt")).unwrap(),
            "# hi\n",
            "a rename keeps what the file held"
        );
    }

    #[test]
    fn a_listing_removes_what_it_no_longer_names() {
        let dir = a_directory("inkstone-buffer-listing-remove");
        let mut buf = listing_of(&dir);
        retype(&mut buf, "docs/\nsrc/\n");

        buf.save(None).unwrap();

        assert!(!dir.join("README.md").exists());
        assert!(!dir.join("Cargo.toml").exists());
        assert!(dir.join("docs").is_dir(), "what is still named stays");
    }

    #[test]
    fn a_directory_goes_only_when_it_is_empty() {
        let dir = a_directory("inkstone-buffer-listing-nonempty");
        std::fs::write(dir.join("src").join("main.rs"), "fn main() {}\n").unwrap();
        let mut buf = listing_of(&dir);
        // `docs` is empty and goes; `src` is not, so the write stops there and
        // says which entry it stopped on.
        retype(&mut buf, "Cargo.toml\nREADME.md\n");

        let err = buf.save(None).unwrap_err().to_string();

        assert!(err.starts_with("src: "), "{err}");
        assert!(
            dir.join("src").join("main.rs").is_file(),
            "nothing was lost"
        );
        assert!(
            !dir.join("docs").exists(),
            "what the plan did reach is done"
        );
        assert!(
            dir.join("README.md").is_file(),
            "and what it did not, is not"
        );
        assert!(dir.join("Cargo.toml").is_file());
    }

    #[test]
    fn writing_the_same_listing_twice_does_nothing_the_second_time() {
        let dir = a_directory("inkstone-buffer-listing-twice");
        let mut buf = listing_of(&dir);
        retype(&mut buf, "docs/\nsrc/\nCargo.toml\nREADME.md\napp.rs\n");
        buf.save(None).unwrap();
        // A second save plans nothing, so the `create_new` of `app.rs` is never
        // reached — which is what makes it safe to have used it.
        buf.save(None).unwrap();
        assert!(dir.join("app.rs").is_file());
    }

    #[test]
    fn a_listing_refuses_a_line_that_is_not_a_name() {
        let dir = a_directory("inkstone-buffer-listing-refusals");

        let cases = [
            (
                "docs/\nsrc/\nsub/name\n",
                "line 3: a name is one name, not a path",
            ),
            (
                "docs/\nsrc/\n..\n",
                "line 3: `.` and `..` are the directory itself",
            ),
            ("docs/\nsrc/\ndocs\n", "line 3: two lines name `docs`"),
            // A line that is only the marker names a directory called nothing.
            ("docs/\nsrc/\n/\n", "line 3: a name cannot be empty"),
        ];
        for (text, expected) in cases {
            let mut buf = listing_of(&dir);
            retype(&mut buf, text);
            let err = buf.save(None).unwrap_err().to_string();
            assert_eq!(err, expected);
        }
        // Nothing was done on the way to any of those refusals.
        assert!(dir.join("docs").is_dir());
        assert!(!dir.join("sub").exists());
    }

    #[test]
    fn a_line_cannot_change_what_kind_of_thing_an_entry_is() {
        let dir = a_directory("inkstone-buffer-listing-kind");
        let mut buf = listing_of(&dir);
        retype(&mut buf, "docs\nsrc/\nCargo.toml\nREADME.md\n");

        let err = buf.save(None).unwrap_err().to_string();
        assert_eq!(
            err,
            "`docs` is a directory, and one line cannot make it a file"
        );
    }

    #[test]
    fn a_listing_is_written_in_place_and_nowhere_else() {
        let dir = a_directory("inkstone-buffer-listing-in-place");
        let mut buf = listing_of(&dir);
        let elsewhere = dir.join("elsewhere");

        let err = buf.save(Some(&elsewhere)).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "E: a listing is written in place, not as {}",
                elsewhere.display()
            )
        );
        assert!(!elsewhere.exists());
        // The name it already has is the one it is written under.
        buf.save(Some(&dir)).unwrap();
    }

    #[test]
    fn a_plan_pairs_only_one_arrival_with_one_removal() {
        let before = |name: &str, is_dir: bool| Entry {
            name: name.to_string(),
            is_dir,
        };
        let original = vec![before("one.txt", false), before("two.txt", false)];

        // Two gone and two arrived is two of each, not two renames: pairing
        // them up would be guessing which name became which.
        let wanted = vec![before("three.txt", false), before("four.txt", false)];
        assert_eq!(
            plan_listing(&original, &wanted).unwrap(),
            vec![
                ListingOp::Remove(before("one.txt", false)),
                ListingOp::Remove(before("two.txt", false)),
                ListingOp::Create(before("three.txt", false)),
                ListingOp::Create(before("four.txt", false)),
            ]
        );

        // One of each is a rename, and nothing else is planned.
        let wanted = vec![before("one.txt", false), before("three.txt", false)];
        assert_eq!(
            plan_listing(&original, &wanted).unwrap(),
            vec![ListingOp::Rename {
                from: "two.txt".to_string(),
                to: "three.txt".to_string(),
            }]
        );

        // A directory and a file are never a rename of one another: the entry
        // that is left is removed and the entry that arrived is made.
        let wanted = vec![before("one.txt", false), before("three", true)];
        assert_eq!(
            plan_listing(&original, &wanted).unwrap(),
            vec![
                ListingOp::Remove(before("two.txt", false)),
                ListingOp::Create(before("three", true)),
            ]
        );

        // The same listing in a different order is not an edit at all.
        let unwanted = vec![before("two.txt", false), before("one.txt", false)];
        assert_eq!(plan_listing(&original, &unwanted).unwrap(), Vec::new());
    }
}
