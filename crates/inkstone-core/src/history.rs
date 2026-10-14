//! The edit history: what `u` and `<C-r>` walk through.
//!
//! # What one step is
//!
//! A step is one *replacement*: the characters that were in
//! `[start, start + removed.len())` are now `inserted`, and the cursor moved
//! from `before` to `after`. The two texts are owned strings and the position
//! is a character index into the buffer as it was *before* the step, so a step
//! stays valid while later steps are recorded on top of it — undoing
//! newest-first always puts back exactly the text that was there.
//!
//! # Grouping
//!
//! A user thinks of the word they just typed as one step, not seven. While a
//! group is open, a step that *continues* the group's entry is folded into it
//! (typing at its end, backspacing into what was just typed, deleting a run one
//! key at a time); anything else starts a new entry. `History::begin_group` and
//! `History::end_group` nest, and the editor opens a group around one command
//! execution and around an insert session — which is what makes a counted `3x`
//! and a whole `i…<Esc>` one `u` each.
//!
//! # The trailing newline
//!
//! [`Buffer`] restores its trailing-newline invariant after every mutation, so
//! a step that removes the buffer's last terminator also brings a newline
//! *back*. That restored newline is part of the step: if it were not, undoing a
//! `dd` on the last line of a file would leave the newline behind and turn the
//! file into a stray empty line. `Change::planned` is where that is accounted
//! for, and it mirrors `Buffer::normalize`'s rule exactly.

use crate::buffer::Buffer;
use crate::cursor::Cursor;

/// One recorded step.
///
/// Everything is private: a step is created from a buffer
/// ([`Change::planned`]), filled in once the edit has run ([`Change::settled`])
/// and handed to [`History`], which is the only thing that knows how to walk
/// the steps backwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Change {
    /// Where the replacement starts, as a character index.
    start: usize,
    /// The text that was in `[start, start + removed.len())`.
    removed: String,
    /// The text that is there now.
    inserted: String,
    /// The cursor before the change; where undo puts it back.
    before: Cursor,
    /// The cursor after the change; where redo puts it back.
    after: Cursor,
    /// Where, inside `inserted`, the next text would be written: a count of
    /// characters from `start`. It is usually at the end of `inserted`, since
    /// typing writes on from the cursor, but `o` leaves the cursor *in front of*
    /// the line break it inserted and the typing lands there instead.
    caret: usize,
    /// Which group of steps this one belongs to, or zero for a step of its own.
    ///
    /// A group whose steps fold into one change does not need this — it *is* one
    /// change. One whose steps cannot fold (they overlap, as a block `c` does)
    /// keeps them as they are and undoes them together, which is what makes a
    /// group one `u` either way.
    group: u32,
    /// Which state of the history this step's result is, by identity rather
    /// than by number. The history hands out the numbers, and the state a write
    /// saved is remembered by one: so `u` back to the saved state is "the same
    /// state", not "a state that looks the same", and no comparing of text is
    /// needed to know.
    state: u64,
}

impl Change {
    /// Describe replacing `[start, end)` with `text` in `buffer`.
    ///
    /// Must be called *before* the replacement is applied: both the text being
    /// removed and the newline this step restores are read from the buffer as
    /// it still is.
    pub(crate) fn planned(
        buffer: &Buffer,
        start: usize,
        end: usize,
        text: &str,
        before: Cursor,
    ) -> Self {
        // Clamped exactly as `Buffer::replace_range` clamps, so the recorded
        // range is the range the replacement actually touches.
        let len = buffer.len_chars();
        let start = start.min(len);
        let end = end.clamp(start, len);
        let inserted = with_restored_newline(buffer, start, end, text);
        let caret = inserted.chars().count();
        Self {
            start,
            removed: buffer.slice_text(start, end),
            inserted,
            before,
            after: before,
            caret,
            group: 0,
            state: 0,
        }
    }

    /// Say where the cursor ended up inside what this step wrote, as a count of
    /// characters from the start of the replacement.
    ///
    /// A deletion has no caret worth speaking of; an insertion's is the cursor
    /// it was given, measured from where the replacement began.
    pub(crate) fn with_caret(mut self, caret: usize) -> Self {
        self.caret = caret.min(self.inserted.chars().count());
        self
    }

    /// Record where the cursor ended up, once the edit has been applied.
    pub(crate) fn settled(mut self, after: Cursor) -> Self {
        self.after = after;
        self
    }

    /// Whether this step only put text in.
    fn is_insert(&self) -> bool {
        self.removed.is_empty() && !self.inserted.is_empty()
    }

    /// Whether this step only took text out.
    fn is_delete(&self) -> bool {
        self.inserted.is_empty() && !self.removed.is_empty()
    }

    /// Fold `next` into this step when the two are one change to a user.
    ///
    /// The four cases are the ones a run of keystrokes actually produces:
    /// typing at the end of what was typed, backspacing into it, deleting a run
    /// one key at a time, and backspacing into text that was already there.
    /// Anything else — a jump to another part of the buffer — starts a new
    /// step, because a single step is a single contiguous range.
    fn absorb(&mut self, next: &Change) -> bool {
        if self.is_insert() && next.is_insert() {
            if next.start == self.start + self.caret {
                // Written where this step left the cursor, which is at the end
                // of what it wrote — unless it was `o`, which leaves the cursor
                // in front of the line break it put in and takes the typing
                // there. Either way it is one run of typing.
                splice(&mut self.inserted, self.caret, 0, &next.inserted);
                self.caret += next.inserted.chars().count();
                self.after = next.after;
                return true;
            }
            return false;
        }
        if self.is_delete() && next.is_delete() {
            if next.start == self.start {
                // The characters after the ones just removed slid into place.
                self.removed.push_str(&next.removed);
                self.after = next.after;
                return true;
            }
            if next.start + next.removed.chars().count() == self.start {
                // Backspacing: the new removal is immediately before this one.
                self.start = next.start;
                self.removed.insert_str(0, &next.removed);
                self.after = next.after;
                return true;
            }
            return false;
        }
        if self.is_insert() && next.is_delete() {
            // Backspacing over text this step just inserted takes it back out
            // again, so the step shrinks rather than splitting in two — and
            // since backspace takes the character *before* the cursor, what
            // goes is the text in front of the caret.
            let removed = next.removed.chars().count();
            if removed <= self.caret && next.start + removed == self.start + self.caret {
                splice(&mut self.inserted, self.caret - removed, removed, "");
                self.caret -= removed;
                self.after = next.after;
                return true;
            }
        }
        false
    }
}

/// The text a replacement leaves in the buffer.
///
/// `Buffer::replace_range` restores the trailing-newline invariant, so a
/// replacement that takes the buffer's last terminator hands one back. This
/// mirrors `Buffer::normalize`: only a `'\n'` at the very end satisfies the
/// invariant, and it comes back only when the replacement left the buffer
/// without one.
fn with_restored_newline(buffer: &Buffer, start: usize, end: usize, text: &str) -> String {
    let takes_the_last_terminator = end == buffer.len_chars()
        && !text.ends_with('\n')
        && !(text.is_empty() && start > 0 && buffer.char_at(start - 1) == Some('\n'));
    if takes_the_last_terminator {
        return format!("{text}\n");
    }
    text.to_string()
}

/// The steps that were applied, and the ones that were undone.
#[derive(Debug, Clone)]
pub struct History {
    /// Applied steps, oldest first.
    past: Vec<Change>,
    /// Steps that were undone, most recently undone last.
    future: Vec<Change>,
    /// The identity of the state an empty history stands for: the file as it
    /// was read, or the empty buffer as it was made.
    base_state: u64,
    /// The identity of the state that was last written out.
    saved_state: u64,
    /// The next identity to hand out. Identities are never reused, so a state
    /// that is edited and then edited back is not the same state again — which
    /// is exactly the rule vim uses to decide whether a buffer is modified.
    next_state: u64,
    /// How many edits have been recorded at all. A command can look at this
    /// before and after running to know whether it changed anything, which is
    /// what `.` needs to remember the last change rather than the last key.
    edits: u64,
    /// How many groups are open. Zero means every step stands alone.
    depth: usize,
    /// Index in `past` of the entry the open group is building, once it has one.
    entry: Option<usize>,
    /// Index in `past` of the first step the outermost open group made. Those
    /// steps become one when the group closes.
    group_start: usize,
    /// The id handed to the group being recorded. Every group gets one of its
    /// own, so a group whose steps cannot fold can still be undone whole.
    group_id: u32,
}

/// Fold a group's steps into the one replacement that has the same effect, or
/// `None` if they overlap in a way one replacement cannot describe.
///
/// Each step knows exactly what it removed and what it inserted, and the text
/// *between* two steps was never touched by either, so it reads the same before
/// and after: it is read from the buffer and copied into both sides. Steps that
/// overlap (a script editing inside text it has just written) are left as they
/// are, which costs an extra `u` rather than undoing something that never
/// happened.
fn coalesce(changes: &[Change], buffer: &Buffer) -> Option<Change> {
    // The steps come in the order they were made, and a command may make them
    // anywhere: a block insert types on every row of a rectangle, so the steps
    // go along the rows and then start again at the top. Ordering them by where
    // they are is what lets the group fold at all — what the fold needs is that
    // the steps do not *overlap*, not that they were made left to right.
    let mut order: Vec<&Change> = changes.iter().collect();
    order.sort_by_key(|change| change.start);
    let mut merged = (*order.first()?).clone();
    for next in &order[1..] {
        // Where the text this step has written ends, in the buffer as it is now.
        let end = merged.start + merged.inserted.chars().count();
        if next.start < end {
            // Overlapping steps cannot be said as one replacement: what the
            // second one removed is not what the first one left. (A block `c`
            // meets this — it deletes a rectangle and types where it was — and
            // its deletion and its typing are therefore two steps of `u`. See the
            // roadmap: folding them needs a real merge of overlapping edits,
            // which is a piece of work rather than a patch.)
            return None;
        }
        let between = buffer.text_range(end, next.start);
        merged.inserted.push_str(&between);
        merged.inserted.push_str(&next.inserted);
        merged.removed.push_str(&between);
        merged.removed.push_str(&next.removed);
        merged.after = next.after;
    }
    merged.caret = merged.inserted.chars().count();
    // The cursor a redo lands on, and the state the group leaves behind, are the
    // ones the *last step made* left — which is where a person was looking.
    merged.after = changes.last()?.after;
    merged.state = changes.last()?.state;
    Some(merged)
}

/// Fold a group of *overlapping* steps into the one replacement they add up to.
///
/// Overlap is what [`coalesce`] cannot read: a step that starts inside what an
/// earlier one wrote removes text the earlier one had put there, so what the
/// group removed is not the sum of what its steps removed. But the steps know
/// enough to *replay* it: taken back in the reverse of the order they were made
/// — each with the coordinates it was made with, which is what undo does — the
/// buffer becomes what it was before the group, and the span can be read there.
/// Then they are applied again, and the span is read as it is now.
///
/// The span is a *superset* of what the group touched: over-covering costs
/// nothing, since the text outside the group's reach is the same before and
/// after, and a replacement that puts it back unchanged is still the same
/// change. Under-covering would lose text.
fn merge_run(changes: &[Change], buffer: &mut Buffer) -> Option<Change> {
    let first = changes.first()?;
    let last = changes.last()?;
    let net: i64 = changes
        .iter()
        .map(|change| {
            change.inserted.chars().count() as i64 - change.removed.chars().count() as i64
        })
        .sum();
    let start = changes.iter().map(|change| change.start).min()?;
    // A bound that is at least as far as any step reaches in either direction.
    let reach = changes
        .iter()
        .map(|change| {
            change.start
                + change
                    .inserted
                    .chars()
                    .count()
                    .max(change.removed.chars().count())
        })
        .max()?;
    let before_len = buffer.len_chars();
    let end_before = reach
        .saturating_add(net.unsigned_abs() as usize)
        .min(before_len);

    // Back to what it was: the steps undone, newest first.
    for change in changes.iter().rev() {
        let inserted = change.inserted.chars().count();
        buffer.replace_range(change.start, change.start + inserted, &change.removed);
    }
    let removed = buffer.slice_text(start, end_before.min(buffer.len_chars()));

    // And forward again, in the order they were made.
    for change in changes {
        let removed = change.removed.chars().count();
        buffer.replace_range(change.start, change.start + removed, &change.inserted);
    }
    if buffer.len_chars() != before_len + net.max(0) as usize
        && buffer.len_chars() as i64 != before_len as i64 + net
    {
        // The replay did not put the buffer back where it was: refuse rather than
        // record a change that does not describe it.
        return None;
    }
    let end_now = start + removed.chars().count() + net.max(0) as usize;
    let end_now = end_now.min(buffer.len_chars());
    let inserted = buffer.slice_text(start, end_now);
    let mut merged = Change {
        start,
        removed,
        inserted,
        before: first.before,
        after: last.after,
        caret: last.caret,
        group: 0,
        state: last.state,
    };
    merged.caret = merged.caret.min(merged.inserted.chars().count());
    Some(merged)
}

/// Replace `remove` characters of `text` at character index `at` with `with`.
fn splice(text: &mut String, at: usize, remove: usize, with: &str) {
    let start = boundary(text, at);
    let end = boundary(text, at + remove);
    text.replace_range(start..end, with);
}

/// The byte index of the `chars`-th character of `text`, or its end.
fn boundary(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

impl Default for History {
    fn default() -> Self {
        Self {
            past: Vec::new(),
            future: Vec::new(),
            depth: 0,
            entry: None,
            group_start: 0,
            group_id: 0,
            // Zero is the state a file that has just been read is in. The first
            // step gets one of its own, so an edit is never mistaken for it.
            base_state: 0,
            saved_state: 0,
            next_state: 1,
            edits: 0,
        }
    }
}

impl History {
    /// An empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget every step, for when the buffer is replaced wholesale (`:e`).
    ///
    /// Steps hold character indices into the document that was being edited, so
    /// they cannot be replayed onto a different one.
    pub fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
        self.entry = None;
    }

    /// Forget every step: the text now is what it was when it arrived — a file
    /// just read, or a buffer just made. It is therefore unmodified, and every
    /// identity that came before is behind it.
    pub(crate) fn reset(&mut self) {
        self.clear();
        self.base_state = self.next_state;
        self.saved_state = self.base_state;
        self.next_state += 1;
    }

    /// How many edits have been recorded. It only ever grows: a command is a
    /// change if this is not the number it was when the command started.
    pub(crate) fn edits(&self) -> u64 {
        self.edits
    }

    /// How many steps can be undone, which is where in the history the editor is.
    ///
    /// The editor keeps the folds in step with this: folds are not text, so they are
    /// not *in* the history, and a fold put on a line that an undo takes away has to
    /// come back with it.
    pub(crate) fn steps(&self) -> usize {
        self.past.len()
    }

    /// Whether the text has been changed since it was last written.
    ///
    /// Not a flag on the buffer: the state the text is in either *is* the state
    /// that was written or it is not, and only the history knows which state
    /// that is. `u` back to it clears this, and an edit that happens to restore
    /// the same text does not.
    pub(crate) fn is_dirty(&self) -> bool {
        self.current_state() != self.saved_state
    }

    /// Which state the text is in now.
    fn current_state(&self) -> u64 {
        self.past.last().map_or(self.base_state, |step| step.state)
    }

    /// Say that the state the text is in now is the one that was written.
    pub(crate) fn mark_saved(&mut self) {
        self.saved_state = self.current_state();
    }

    /// Whether nothing has been changed at all, undoing included: the buffer is
    /// as it was when it arrived. A flag on the buffer cannot say this, because
    /// `Buffer::empty` is born "modified".
    pub(crate) fn is_untouched(&self) -> bool {
        self.past.is_empty() && self.future.is_empty()
    }

    /// Start a group: the steps recorded before it closes collapse into one.
    ///
    /// Groups nest — a command opens one inside the insert session that is
    /// already open — and only the outermost one decides what a `u` takes back.
    pub(crate) fn begin_group(&mut self) {
        if self.depth == 0 {
            self.group_start = self.past.len();
            // Every group gets an id of its own, so a group whose steps cannot
            // fold can still be undone as one.
            self.group_id += 1;
        }
        self.depth += 1;
    }

    /// Close a group, folding its steps into one against `buffer` so a single
    /// `u` takes the whole group back.
    ///
    /// The steps a group made usually chain into one entry already, since
    /// [`Change::absorb`] joins edits that touch the same place. When they do
    /// not — an `o` that opens a line and the text typed into it are two
    /// separate edits — the group would otherwise come apart under `u`, leaving
    /// half of it behind.
    pub(crate) fn end_group(&mut self, buffer: &mut Buffer) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth > 0 {
            return;
        }
        self.entry = None;
        let start = self.group_start;
        if self.past.len() > start + 1 {
            if let Some(merged) = coalesce(&self.past[start..], buffer) {
                self.past.truncate(start);
                // The folded change stands alone: there is nothing left to undo
                // together.
                self.past.push(Change { group: 0, ..merged });
            } else if let Some(merged) = merge_run(&self.past[start..], buffer) {
                // Steps that overlap cannot be *read* as one replacement, but the
                // group can still *become* one: the buffer is taken back to what it
                // was before the group, the span is read there, and the steps are
                // applied again.
                self.past.truncate(start);
                self.past.push(Change { group: 0, ..merged });
            }
            // When even that fails, the steps stay as they are, and undo takes
            // them back together: the id they were given is what says so.
        }
    }

    /// Record `change`, which has already been applied to the buffer.
    pub(crate) fn push(&mut self, mut change: Change) {
        // A step made inside a group carries that group's id, which is what lets
        // the group be undone whole when its steps cannot fold into one.
        if self.depth > 0 {
            change.group = self.group_id;
        }
        if change.removed == change.inserted {
            // The edit left no trace in the text: an empty line "deleted" from
            // an empty buffer is the whole of it. There is nothing to undo.
            return;
        }
        // Anything that was undone is unreachable from here on.
        self.future.clear();
        if let Some(index) = self.entry {
            // Steps of *different* groups do not join, however adjacent they are:
            // a group that cannot fold is undone as a run of steps, and a run is
            // only a run if nothing outside it is in it.
            let same_group = self.past[index].group == change.group;
            if same_group && self.past[index].absorb(&change) {
                // Growing a step makes a *new* state out of the one it grew
                // from, even when the change joined it: a state that has been
                // edited is not the state that was written.
                self.past[index].state = self.take_state();
                self.edits += 1;
                return;
            }
        }
        let mut change = change;
        change.state = self.take_state();
        self.past.push(change);
        self.edits += 1;
        if self.depth > 0 {
            self.entry = Some(self.past.len() - 1);
        }
    }

    /// The next state identity.
    fn take_state(&mut self) -> u64 {
        let state = self.next_state;
        self.next_state += 1;
        state
    }

    /// Undo the newest step against `buffer`, returning the cursor it wants.
    ///
    /// `None` means there is nothing left to undo.
    pub(crate) fn undo(&mut self, buffer: &mut Buffer) -> Option<Cursor> {
        let change = self.past.pop()?;
        // A group that could not fold is undone whole: its steps are undone in
        // the reverse of the order they were made, which is what makes each one's
        // coordinates valid again as it is reached.
        let group = change.group;
        let mut cursor = self.undo_one(change, buffer);
        if group != 0 {
            while self.past.last().is_some_and(|next| next.group == group) {
                let next = self.past.pop().expect("just looked at it");
                cursor = self.undo_one(next, buffer);
            }
        }
        // The step that was just undone is finished; the next edit starts a new one.
        self.entry = None;
        Some(cursor)
    }

    /// Undo one step, and remember it for a redo.
    fn undo_one(&mut self, change: Change, buffer: &mut Buffer) -> Cursor {
        let inserted = change.inserted.chars().count();
        buffer.replace_range(change.start, change.start + inserted, &change.removed);
        let cursor = change.before;
        self.future.push(change);
        cursor
    }

    /// Redo the step that was undone last, against `buffer`.
    ///
    /// `None` means there is nothing to redo.
    pub(crate) fn redo(&mut self, buffer: &mut Buffer) -> Option<Cursor> {
        let change = self.future.pop()?;
        let group = change.group;
        // Redo is the mirror of undo: the steps of a group go back in the order
        // they were made.
        let mut cursor = self.redo_one(change, buffer);
        if group != 0 {
            while self.future.last().is_some_and(|next| next.group == group) {
                let next = self.future.pop().expect("just looked at it");
                cursor = self.redo_one(next, buffer);
            }
        }
        self.entry = None;
        Some(cursor)
    }

    /// Redo one step, and put it back where undo found it.
    fn redo_one(&mut self, change: Change, buffer: &mut Buffer) -> Cursor {
        let removed = change.removed.chars().count();
        buffer.replace_range(change.start, change.start + removed, &change.inserted);
        let cursor = change.after;
        self.past.push(change);
        cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A step that only added text, as typing does.
    fn insert(start: usize, text: &str) -> Change {
        Change {
            start,
            removed: String::new(),
            caret: text.chars().count(),
            group: 0,
            state: 0,
            inserted: text.to_string(),
            before: Cursor::default(),
            after: Cursor::default(),
        }
    }

    /// A step that only took text out, as `x` or `dd` does.
    fn delete(start: usize, text: &str) -> Change {
        Change {
            start,
            removed: text.to_string(),
            caret: 0,
            group: 0,
            state: 0,
            inserted: String::new(),
            before: Cursor::default(),
            after: Cursor::default(),
        }
    }

    /// A history holding one step, applied to `buffer`.
    fn recorded(buffer: &mut Buffer, start: usize, end: usize, text: &str) -> History {
        let mut history = History::new();
        let change = Change::planned(buffer, start, end, text, Cursor::default());
        buffer.replace_range(start, end, text);
        history.push(change.settled(Cursor::default()));
        history
    }

    #[test]
    fn typing_collapses_into_one_step() {
        let mut typed = insert(0, "he");
        assert!(typed.absorb(&insert(2, "llo")));
        assert_eq!(typed.inserted, "hello");
        // ...but only while the text keeps growing at its end.
        assert!(!typed.absorb(&insert(0, "x")));
        assert!(!typed.absorb(&insert(7, "!")));
    }

    #[test]
    fn backspacing_into_what_was_just_typed_folds_back_in() {
        let mut typed = insert(0, "hello");
        assert!(typed.absorb(&delete(4, "o")));
        assert_eq!(typed.inserted, "hell");
        // Backspacing away everything typed leaves an empty step behind, which
        // `push` refuses to record.
        assert!(typed.absorb(&delete(0, "hell")));
        assert_eq!(typed.inserted, "");
        assert!(!typed.is_insert() && !typed.is_delete());
    }

    #[test]
    fn a_run_of_deletes_merges_in_both_directions() {
        // `3x`: the first two removals happen at the same index, because the
        // characters after them slide into place; the third one happens one
        // column to the left, because the cursor was clamped back onto the
        // last character.
        let mut run = delete(4, "e");
        assert!(run.absorb(&delete(4, "f")));
        assert_eq!(run.removed, "ef");
        assert!(run.absorb(&delete(3, "d")));
        assert_eq!((run.start, run.removed.as_str()), (3, "def"));
        // Three backspaces: each removal is immediately before the last.
        let mut back = delete(4, "c");
        assert!(back.absorb(&delete(3, "b")));
        assert_eq!(back.start, 3);
        assert_eq!(back.removed, "bc");
        assert!(back.absorb(&delete(2, "a")));
        assert_eq!((back.start, back.removed.as_str()), (2, "abc"));
    }

    #[test]
    fn an_edit_elsewhere_starts_a_new_step() {
        let mut typed = insert(0, "ab");
        assert!(!typed.absorb(&delete(9, "x")));
        assert!(!typed.absorb(&insert(9, "x")));
        assert!(!delete(0, "ab").absorb(&insert(5, "x")));
        assert!(!delete(0, "ab").absorb(&delete(9, "x")));
        // Overlapping but not adjacent: a backspace into text that was there
        // before the insertion is a step of its own.
        let mut typed = insert(2, "ab");
        assert!(!typed.absorb(&delete(0, "x")));
    }

    #[test]
    fn a_group_folds_consecutive_steps_into_one() {
        let mut buffer = Buffer::from_text("\n");
        let mut history = History::new();
        history.begin_group();
        for (start, text) in [(0, "a"), (1, "b"), (2, "c")] {
            let change = Change::planned(&buffer, start, start, text, Cursor::default());
            buffer.replace_range(start, start, text);
            history.push(change.settled(Cursor::default()));
        }
        history.end_group(&mut buffer);
        assert_eq!(buffer.text(), "abc\n");
        assert_eq!(history.past.len(), 1, "the group is one step");

        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.text(), "\n");
        assert_eq!(history.past.len(), 0);
    }

    #[test]
    fn a_group_folds_steps_that_do_not_chain_into_one_too() {
        // What `o` and a line of typing do: the line break goes in at one place
        // and the text at the next one, so they do not chain into a single
        // entry. One `u` must still take back both.
        let mut buffer = Buffer::from_text("one\n");
        let mut history = History::new();
        history.begin_group();
        let open = Change::planned(&buffer, 3, 3, "\n", Cursor::default());
        buffer.replace_range(3, 3, "\n");
        history.push(open.settled(Cursor::default()));
        let typed = Change::planned(&buffer, 4, 4, "hello", Cursor::default());
        buffer.replace_range(4, 4, "hello");
        history.push(typed.settled(Cursor::default()));
        history.end_group(&mut buffer);

        assert_eq!(buffer.text(), "one\nhello\n");
        assert_eq!(history.past.len(), 1, "the group is one step");
        assert!(!history.past[0].removed.contains("host"), "sanity");
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.text(), "one\n", "the whole group came back out");
        assert!(history.redo(&mut buffer).is_some());
        assert_eq!(buffer.text(), "one\nhello\n");
    }

    #[test]
    fn a_group_that_removes_and_inserts_folds_to_one_replacement() {
        // `cw`: the word goes and other text takes its place, in two steps that
        // never shared a position.
        let mut buffer = Buffer::from_text("one two three\n");
        let mut history = History::new();
        history.begin_group();
        let removed = Change::planned(&buffer, 4, 7, "", Cursor::default());
        buffer.replace_range(4, 7, "");
        history.push(removed.settled(Cursor::default()));
        let typed = Change::planned(&buffer, 4, 4, "2", Cursor::default());
        buffer.replace_range(4, 4, "2");
        history.push(typed.settled(Cursor::default()));
        history.end_group(&mut buffer);

        assert_eq!(buffer.text(), "one 2 three\n");
        assert_eq!(history.past.len(), 1);
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.text(), "one two three\n", "the word is back");
    }

    #[test]
    fn a_group_folds_steps_made_in_any_order() {
        // A command may make its steps anywhere: a block insert types on every
        // row of a rectangle and comes back to the first one for the next key, so
        // the steps go along the rows and then start again at the top. What the
        // fold needs is that they do not *overlap* — not that they were made left
        // to right — and a group is one `u` however they were made.
        let mut buffer = Buffer::from_text("aaa\nbbb\n");
        let mut history = History::new();
        history.begin_group();
        // Made out of order, and each offset is where the buffer is *then*.
        for (start, text) in [(0usize, "1"), (10, "3\n"), (5, "2")] {
            let change = Change::planned(&buffer, start, start, text, Cursor::default());
            buffer.replace_range(start, start, text);
            history.push(change.settled(Cursor::default()));
        }
        history.end_group(&mut buffer);
        assert_eq!(buffer.text(), "1aaa\n2bbb\n3\n", "all three went in");
        assert_eq!(history.past.len(), 1, "one step, not three");
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.text(), "aaa\nbbb\n", "and one `u` takes it all back");
    }

    #[test]
    fn a_group_of_overlapping_steps_folds_by_being_replayed() {
        // Steps that touch the same place cannot be *read* as one replacement:
        // what the second one removed is not what the first one left. They can
        // still *become* one — the group is replayed backwards to what the buffer
        // was before it, the span is read there, and the steps are applied again.
        let mut buffer = Buffer::from_text("abc\n");
        let mut history = History::new();
        history.begin_group();
        for (start, end, text) in [(0usize, 1usize, "X"), (0, 1, "Y")] {
            let change = Change::planned(&buffer, start, end, text, Cursor::default());
            buffer.replace_range(start, end, text);
            history.push(change.settled(Cursor::default()));
        }
        history.end_group(&mut buffer);
        assert_eq!(buffer.text(), "Ybc\n");
        assert_eq!(history.past.len(), 1, "one step, overlapping or not");
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.text(), "abc\n", "and one `u` takes the group back");
        assert!(history.redo(&mut buffer).is_some());
        assert_eq!(buffer.text(), "Ybc\n", "redo puts the group back");
    }

    #[test]
    fn undo_and_redo_restore_the_text() {
        let mut buffer = Buffer::from_text("one\ntwo\n");
        // `dd` on the second line, which takes its terminator with it.
        let mut history = recorded(&mut buffer, 4, 8, "");
        assert_eq!(buffer.text(), "one\n");

        assert_eq!(history.undo(&mut buffer), Some(Cursor::default()));
        assert_eq!(buffer.text(), "one\ntwo\n", "the terminator came back too");
        assert_eq!(history.redo(&mut buffer), Some(Cursor::default()));
        assert_eq!(buffer.text(), "one\n");
        assert_eq!(history.redo(&mut buffer), None, "nothing left to redo");
    }

    #[test]
    fn a_step_that_takes_the_last_terminator_owns_the_restored_newline() {
        // `dd` on the only line: the buffer cannot be empty, so the newline
        // that comes back belongs to this step.
        let mut buffer = Buffer::from_text("abc\n");
        let mut history = recorded(&mut buffer, 0, 4, "");
        assert_eq!(buffer.text(), "\n");

        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.text(), "abc\n", "not abc\n\n");
        assert!(history.redo(&mut buffer).is_some());
        assert_eq!(buffer.text(), "\n");
    }

    #[test]
    fn a_step_that_keeps_a_newline_restores_nothing() {
        let buffer = Buffer::from_text("abc\n");
        assert_eq!(with_restored_newline(&buffer, 0, 1, ""), "");
        assert_eq!(with_restored_newline(&buffer, 3, 4, ""), "\n");
        // Inserting text that is not a newline at the very end does not change
        // the fact that the buffer already ends with one.
        assert_eq!(with_restored_newline(&buffer, 3, 3, "x"), "x");
    }

    #[test]
    fn a_step_that_changes_nothing_is_not_recorded() {
        let mut buffer = Buffer::from_text("\n");
        let mut history = History::new();
        // `dd` on the empty last line removes the newline and gets it back.
        let change = Change::planned(&buffer, 0, 1, "", Cursor::default());
        buffer.replace_range(0, 1, "");
        assert_eq!(buffer.text(), "\n");
        history.push(change.settled(Cursor::default()));
        assert!(
            history.undo(&mut buffer).is_none(),
            "nothing changed, so there is nothing to undo"
        );
        assert_eq!(buffer.text(), "\n");
    }

    #[test]
    fn a_new_step_drops_what_was_undone() {
        let mut buffer = Buffer::from_text("abc\n");
        let mut history = recorded(&mut buffer, 0, 1, "x");
        assert_eq!(buffer.text(), "xbc\n");
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.text(), "abc\n");

        // Recording a new step makes the undone one unreachable.
        let change = Change::planned(&buffer, 0, 0, "z", Cursor::default());
        buffer.replace_range(0, 0, "z");
        history.push(change.settled(Cursor::default()));
        assert!(
            history.redo(&mut buffer).is_none(),
            "the undone step is gone for good"
        );
        assert_eq!(buffer.text(), "zabc\n", "and redo left the text alone");
    }

    #[test]
    fn clear_forgets_everything() {
        let mut buffer = Buffer::from_text("abc\n");
        let mut history = recorded(&mut buffer, 0, 3, "");
        history.clear();
        assert_eq!(history.undo(&mut buffer), None, "there is nothing to undo");
        assert_eq!(
            history.redo(&mut buffer),
            None,
            "and nothing was put back on the redo stack"
        );
        assert_eq!(buffer.text(), "\n", "the buffer is untouched");
    }
}
