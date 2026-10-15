use crate::substitute::Substitute;

use super::*;

impl Editor {
    /// Start asking about each match of `spec`, one at a time.
    ///
    /// Everything the answers do belongs to one history group: the one the
    /// command that started this is running in, which `run_command` leaves open
    /// while questions are up. `finish_confirm` closes it, so a `:s///c` is one
    /// `u` however many matches were answered — the same as a `:s` that never
    /// asked.
    pub(crate) fn start_confirm(
        &mut self,
        spec: &Substitute,
    ) -> Result<(), crate::search::PatternError> {
        let pattern = regex::RegexBuilder::new(&spec.translated)
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
    pub(super) fn advance_confirm(&mut self) {
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
            match state.pattern.captures(&rest) {
                Some(caps) => {
                    // What is asked about is the part of the match the pattern
                    // reports, which `\zs` and `\ze` can make shorter than the
                    // text the engine matched.
                    let (start, end) = state.spec.extent.span(&caps);
                    let start = state.from + rest[..start].chars().count();
                    let end = state.from + rest[..end].chars().count();
                    let replacement = crate::substitute::expand(
                        &caps,
                        &state.spec.extent,
                        state.spec.replacement.as_str(),
                        &rest,
                    );
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
    pub(crate) fn answer_confirm(&mut self, answer: &str) {
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
    pub(super) fn finish_confirm_all(&mut self) {
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
    pub(super) fn finish_confirm(&mut self) {
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
}
