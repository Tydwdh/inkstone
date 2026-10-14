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
    /// Every state the text has been in, in the order they were *made* — which
    /// is not the order they are related in. Undoing and then editing again adds
    /// a state whose parent is an older one, and what comes out is the undo
    /// *tree* vim has rather than a stack: nothing is thrown away, and `g-` and
    /// `g+` walk the states in the order they were made whatever branch they are
    /// on. `states[0]` is the state the buffer arrived in, which has no change
    /// of its own.
    states: Vec<State>,
    /// Which of them the text is in now.
    current: usize,
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
    /// Index in `states` of the first state the outermost open group made.
    group_start: usize,
    /// The id handed to the group being recorded. Every group gets one of its
    /// own, so a group whose steps cannot fold can still be undone whole.
    group_id: u32,
}

/// One state the text has been in.
///
/// The change is what turned the parent into it, and the parent is what undo
/// goes back to. A state is an *identity* as well as a place in the tree: the
/// identity is what `is_dirty` compares, and it is handed out fresh whenever a
/// step is grown, because a state that has been edited is not the state that was
/// written.
#[derive(Debug, Clone)]
struct State {
    /// The step that produced this state; `None` only for the first.
    change: Option<Change>,
    /// The state it was made from; `None` only for the first.
    from: Option<usize>,
    /// When it was made, for `:undolist`.
    at: std::time::Instant,
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
            states: vec![State {
                change: None,
                from: None,
                at: std::time::Instant::now(),
            }],
            current: 0,
            base_state: 0,
            saved_state: 0,
            next_state: 1,
            edits: 0,
            depth: 0,
            group_start: 0,
            group_id: 0,
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
        self.states.truncate(1);
        self.states[0].change = None;
        self.states[0].from = None;
        self.current = 0;
    }

    /// Forget every step: the text now is what it was when it arrived — a file
    /// just read, or a buffer just made. It is therefore unmodified, and every
    /// identity that came before is behind it.
    pub(crate) fn reset(&mut self) {
        self.clear();
        self.base_state = self.take_state();
        self.saved_state = self.base_state;
        self.states[0].at = std::time::Instant::now();
    }

    /// How many edits have been recorded. It only ever grows: a command is a
    /// change if this is not the number it was when the command started.
    pub(crate) fn edits(&self) -> u64 {
        self.edits
    }

    /// Which state the text is in: an index into the states, in the order they
    /// were made.
    ///
    /// The editor keeps the folds in step with this: folds are not text, so they
    /// are not *in* the history, and a fold put on a line that an undo takes away
    /// has to come back with it. One entry per state, and never thrown away —
    /// a state that an undo left behind is still reachable by `g-`.
    pub(crate) fn state_index(&self) -> usize {
        self.current
    }

    /// How many states there are, the one the buffer arrived in included. Only
    /// the tests ask: what the editor needs is which state it is in.
    #[cfg(test)]
    pub(crate) fn state_count(&self) -> usize {
        self.states.len()
    }

    /// Every state that nothing else was made from, with its index and how long
    /// ago it was made: what `:undolist` shows.
    pub(crate) fn leaves(&self) -> Vec<(usize, std::time::Duration)> {
        let mut parents = vec![false; self.states.len()];
        for state in &self.states {
            if let Some(from) = state.from {
                parents[from] = true;
            }
        }
        let now = std::time::Instant::now();
        self.states
            .iter()
            .enumerate()
            .filter(|(index, state)| state.change.is_some() && !parents[*index])
            .map(|(index, state)| (index, now.duration_since(state.at)))
            .collect()
    }

    /// How many changes deep a state is: the length of the path from the state
    /// the buffer arrived in.
    pub(crate) fn depth_of(&self, index: usize) -> usize {
        let mut depth = 0;
        let mut at = self.states.get(index).and_then(|state| state.from);
        while let Some(parent) = at {
            depth += 1;
            at = self.states[parent].from;
        }
        depth
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

    /// Which state the text is in now, by identity.
    fn current_state(&self) -> u64 {
        self.states[self.current]
            .change
            .as_ref()
            .map_or(self.base_state, |change| change.state)
    }

    /// Say that the state the text is in now is the one that was written.
    pub(crate) fn mark_saved(&mut self) {
        self.saved_state = self.current_state();
    }

    /// Whether nothing has been changed at all, undoing included: the buffer is
    /// as it was when it arrived. A flag on the buffer cannot say this, because
    /// `Buffer::empty` is born "modified".
    pub(crate) fn is_untouched(&self) -> bool {
        self.states.len() == 1
    }

    /// Start a group: the steps recorded before it closes collapse into one.
    ///
    /// Groups nest — a command opens one inside the insert session that is
    /// already open — and only the outermost one decides what a `u` takes back.
    pub(crate) fn begin_group(&mut self) {
        if self.depth == 0 {
            self.group_start = self.states.len();
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
        let start = self.group_start;
        if self.states.len() > start + 1 {
            let steps: Vec<Change> = self.states[start..]
                .iter()
                .filter_map(|state| state.change.clone())
                .collect();
            let folded = coalesce(&steps, buffer).or_else(|| merge_run(&steps, buffer));
            if let Some(merged) = folded {
                // The states of the group become one state: the buffer is where
                // the last of them left it, so the parent is the state the group
                // started from, and the merged change says the whole of what it
                // did.
                let from = self.states[start].from;
                self.states.truncate(start);
                let mut merged = merged;
                merged.group = 0;
                merged.state = self.take_state();
                self.states.push(State {
                    change: Some(merged),
                    from,
                    at: std::time::Instant::now(),
                });
                self.current = self.states.len() - 1;
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
        // A step that continues the one before it joins it, which is what makes
        // typing a word one step rather than seven. Only inside a group: two
        // commands that happen to touch the same place are two `u`, however
        // adjacent they are.
        if self.depth > 0 {
            let same_group = self.states[self.current]
                .change
                .as_ref()
                .is_some_and(|before| before.group == change.group);
            if same_group {
                let absorbed = self.states[self.current]
                    .change
                    .as_mut()
                    .is_some_and(|before| before.absorb(&change));
                if absorbed {
                    // Growing a step makes a *new* state out of the one it grew
                    // from, even when the change joined it: a state that has been
                    // edited is not the state that was written.
                    let identity = self.take_state();
                    if let Some(before) = self.states[self.current].change.as_mut() {
                        before.state = identity;
                    }
                    self.edits += 1;
                    return;
                }
            }
        }
        change.state = self.take_state();
        self.states.push(State {
            change: Some(change),
            from: Some(self.current),
            at: std::time::Instant::now(),
        });
        self.current = self.states.len() - 1;
        self.edits += 1;
    }

    /// The next state identity.
    fn take_state(&mut self) -> u64 {
        let state = self.next_state;
        self.next_state += 1;
        state
    }

    /// Undo the step that made the state the text is in, against `buffer`.
    ///
    /// `None` means there is nothing left to undo — the text is in the state the
    /// buffer arrived in.
    pub(crate) fn undo(&mut self, buffer: &mut Buffer) -> Option<Cursor> {
        let state = self.states.get(self.current)?;
        let change = state.change.clone()?;
        let from = state.from?;
        // A group that could not fold is undone whole: its steps are undone in
        // the reverse of the order they were made, which is what makes each one's
        // coordinates valid again as it is reached.
        let group = change.group;
        let mut cursor = self.undo_one(change, buffer);
        self.current = from;
        // A group that could not fold keeps its id, and its steps are the ones
        // between here and where the group started: they go back together.
        if group != 0 {
            while let Some((change, from)) = self.states.get(self.current).and_then(|state| {
                let change = state.change.clone().filter(|next| next.group == group)?;
                Some((change, state.from?))
            }) {
                cursor = self.undo_one(change, buffer);
                self.current = from;
            }
        }
        Some(cursor)
    }

    /// Undo one step against the buffer.
    fn undo_one(&mut self, change: Change, buffer: &mut Buffer) -> Cursor {
        let inserted = change.inserted.chars().count();
        buffer.replace_range(change.start, change.start + inserted, &change.removed);
        change.before
    }

    /// Redo the step that was undone last, against `buffer`.
    ///
    /// `None` means there is nothing to redo. A state may have been made from
    /// several others — the one that was undone, and one an edit made after it —
    /// and redo takes the one made *last*, which is what makes `u` then an edit
    /// then `<C-r>` continue the branch the person was on.
    pub(crate) fn redo(&mut self, buffer: &mut Buffer) -> Option<Cursor> {
        let child = self.newest_child(self.current)?;
        let change = self.states[child].change.clone().expect("a child has one");
        let group = change.group;
        let mut cursor = self.redo_one(change, buffer);
        self.current = child;
        // Redo is the mirror of undo: the steps of a group go back in the order
        // they were made.
        if group != 0 {
            while let Some((next, change)) = self.newest_child(self.current).and_then(|next| {
                let change = self.states[next]
                    .change
                    .clone()
                    .filter(|change| change.group == group)?;
                Some((next, change))
            }) {
                cursor = self.redo_one(change, buffer);
                self.current = next;
            }
        }
        Some(cursor)
    }

    /// Redo one step against the buffer.
    fn redo_one(&mut self, change: Change, buffer: &mut Buffer) -> Cursor {
        let removed = change.removed.chars().count();
        buffer.replace_range(change.start, change.start + removed, &change.inserted);
        change.after
    }

    /// The state made from `parent` most recently, if any were.
    fn newest_child(&self, parent: usize) -> Option<usize> {
        self.states
            .iter()
            .enumerate()
            .filter(|(_, state)| state.from == Some(parent))
            .map(|(index, _)| index)
            .next_back()
    }

    /// Go to the state at `target`, whatever branch it is on: `g-`, `g+`,
    /// `:earlier` and `:later` are all this, with the target worked out by the
    /// caller.
    ///
    /// The way there is the way a tree is walked: up from where the text is to
    /// the deepest state the two have in common, and then down from there to the
    /// target. States are crossed one at a time, so a group whose steps could not
    /// fold is crossed in pieces — `u` is what takes such a group back whole.
    pub(crate) fn goto(&mut self, target: usize, buffer: &mut Buffer) -> Option<Cursor> {
        if target >= self.states.len() || target == self.current {
            return None;
        }
        let mut down = Vec::new();
        let mut at = Some(target);
        while let Some(index) = at {
            down.push(index);
            if index == self.current {
                break;
            }
            at = self.states[index].from;
        }
        // `down` ends at the deepest shared state. Everything after it is the
        // way to the target, and the rest is the way back from where the text is.
        let common = *down.last()?;
        let mut cursor = None;
        while self.current != common {
            let state = &self.states[self.current];
            let change = state.change.clone()?;
            let from = state.from?;
            cursor = Some(self.undo_one(change, buffer));
            self.current = from;
        }
        for index in down.iter().rev().skip(1) {
            let change = self.states[*index].change.clone()?;
            cursor = Some(self.redo_one(change, buffer));
            self.current = *index;
        }
        cursor
    }

    /// The state about `offset` older (or newer) than the one the text is in:
    /// what `:earlier 10s` and `:later 1h` go to.
    ///
    /// Clamped the way vim's are: nothing that old goes to the state the buffer
    /// arrived in, and nothing that new goes to the newest state there is. The
    /// reading is from the current state's time rather than from now, which is
    /// what makes the two commands inverse to each other.
    pub(crate) fn state_about(&self, offset: std::time::Duration, forward: bool) -> usize {
        let from = self.states[self.current].at;
        if forward {
            let target = from + offset;
            self.states
                .iter()
                .enumerate()
                .find(|(index, state)| *index > self.current && state.at >= target)
                .map(|(index, _)| index)
                .unwrap_or(self.states.len() - 1)
        } else {
            let target = from.checked_sub(offset).unwrap_or(from);
            self.states
                .iter()
                .enumerate()
                .filter(|(index, state)| *index < self.current && state.at <= target)
                .map(|(index, _)| index)
                .next_back()
                .unwrap_or(0)
        }
    }

    /// The state at `index`, if there is one — `:undo {number}`.
    pub(crate) fn has_state(&self, index: usize) -> bool {
        index < self.states.len()
    }

    /// The state `count` steps older in the order they were made, if there is
    /// one: what `g-` goes to.
    pub(crate) fn earlier(&self, count: usize) -> Option<usize> {
        self.current.checked_sub(count).filter(|index| *index > 0)
    }

    /// The state `count` steps newer in the order they were made, if there is
    /// one: what `g+` goes to.
    pub(crate) fn later(&self, count: usize) -> Option<usize> {
        let index = self.current + count;
        (index < self.states.len()).then_some(index)
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
        assert_eq!(history.state_count() - 1, 1, "the group is one step");

        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.text(), "\n", "and one `u` takes the group back");
        assert_eq!(
            history.state_count() - 1,
            1,
            "the state is still there: an undo moves, it does not throw away"
        );
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
        assert_eq!(history.state_count() - 1, 1, "the group is one step");
        assert!(
            !history.states[1]
                .change
                .as_ref()
                .expect("a change")
                .removed
                .contains("host"),
            "sanity"
        );
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
        assert_eq!(history.state_count() - 1, 1);
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
        assert_eq!(history.state_count() - 1, 1, "one step, not three");
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
        assert_eq!(history.state_count() - 1, 1, "one step, overlapping or not");
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
    fn a_new_step_keeps_what_was_undone() {
        // The old linear history threw the undone step away the moment an edit
        // was made after it. The tree keeps it: the edit starts a new branch, and
        // `g-` walks the states in the order they were made, which is how a
        // person gets back to what they undid.
        let mut buffer = Buffer::from_text("abc\n");
        let mut history = recorded(&mut buffer, 0, 1, "x");
        assert_eq!(buffer.text(), "xbc\n");
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.text(), "abc\n");

        let change = Change::planned(&buffer, 0, 0, "z", Cursor::default());
        buffer.replace_range(0, 0, "z");
        history.push(change.settled(Cursor::default()));
        assert_eq!(buffer.text(), "zabc\n");
        assert!(
            history.redo(&mut buffer).is_none(),
            "there is nothing *after* this state: the edit started a branch"
        );
        assert_eq!(buffer.text(), "zabc\n", "so redo left the text alone");

        // The state that was undone is one step back in the order they were
        // made, and `g-` goes there.
        let earlier = history.earlier(1).expect("the state that was undone");
        assert!(history.goto(earlier, &mut buffer).is_some());
        assert_eq!(buffer.text(), "xbc\n", "the branch is still reachable");
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
