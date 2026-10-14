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
    /// The file changed on disk since this buffer read it, so writing would throw
    /// away somebody else's work.
    #[error("E13: File exists (add ! to override): {}", .0.display())]
    Changed(PathBuf),
}

/// An editable document.
#[derive(Debug, Clone)]
pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    /// What the file looked like when this buffer last read or wrote it: its
    /// length and its modification time. `None` means there is nothing on disk to
    /// compare with — a scratch buffer, or a path that did not exist.
    stamp: Option<(u64, std::time::SystemTime)>,
}

impl Buffer {
    /// A new, unnamed, empty buffer (one empty line).
    pub fn empty() -> Self {
        Self {
            rope: Rope::from("\n"),
            path: None,
            stamp: None,
        }
    }

    /// What the file on disk looks like now, if it is there at all.
    fn stamp_of(path: &Path) -> Option<(u64, std::time::SystemTime)> {
        let metadata = std::fs::metadata(path).ok()?;
        Some((metadata.len(), metadata.modified().ok()?))
    }

    /// Load `path`, or create an empty buffer bound to it if it does not exist.
    ///
    /// Errors when the file exists but cannot be read, or is not UTF-8.
    pub fn from_path(path: &Path) -> Result<Self, BufferError> {
        if !path.exists() {
            return Ok(Self {
                rope: Rope::from("\n"),
                path: Some(path.to_path_buf()),
                stamp: None,
            });
        }
        let bytes = std::fs::read(path)?;
        let text =
            String::from_utf8(bytes).map_err(|_| BufferError::NotUtf8(path.to_path_buf()))?;
        let mut buf = Self {
            rope: Rope::from_str(&text),
            path: Some(path.to_path_buf()),
            stamp: Self::stamp_of(path),
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
        let target = match path {
            Some(p) => p.to_path_buf(),
            None => self.path.clone().ok_or(BufferError::NoPath)?,
        };
        if !force {
            let on_disk = Self::stamp_of(&target);
            if on_disk.is_some() && on_disk != self.stamp {
                return Err(BufferError::Changed(target));
            }
        }
        std::fs::write(&target, self.text())?;
        self.path = Some(target.clone());
        self.stamp = Self::stamp_of(&target);
        Ok(target)
    }

    // -- accessors ----------------------------------------------------------

    /// Where this buffer came from, if anywhere. `None` means a scratch buffer.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The name to show in the status line.
    pub fn display_name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
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
}
