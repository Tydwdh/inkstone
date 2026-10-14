//! Key notation, key parsing and the keymap.
//!
//! Keys are written in vim-ish angle notation: `h`, `0`, `<Esc>`, `<CR>`,
//! `<C-w>`, `<A-Left>`. A *sequence* (`dd`, `gg`) is just several keys written
//! back to back; internally the encoded forms are joined with a space, so `dd`
//! is looked up as `"d d"`.
//!
//! Encoding is total: [`encode_key`] always produces something [`parse_key`]
//! accepts, which is what lets mappings round-trip through Lua.

use std::collections::HashMap;

use inkstone_rpc::{KeyCode, KeyMsg, Mod};

use crate::mode::Mode;

/// The separator between encoded keys in a sequence.
const SEQ_SEP: char = ' ';

/// Key notation that does not parse. The message says what was wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("bad key notation: {0}")]
pub struct KeyParseError(pub String);

/// What a key sequence resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The sequence is bound; run these commands in order.
    Exact(Binding),
    /// The sequence is not bound, but a longer one starting with it is.
    Prefix,
    /// Nothing matches.
    None,
}

/// The command sequence a key bindings runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Command names, resolved against the registry and then against Lua.
    pub commands: Vec<String>,
}

impl Binding {
    /// A binding that runs `commands` in order.
    pub fn new(commands: Vec<String>) -> Self {
        Self { commands }
    }
}

/// Bindings, keyed by mode and encoded key sequence.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    maps: HashMap<(Mode, String), Binding>,
}

impl Keymap {
    /// An empty keymap.
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in vim-like bindings.
    pub(crate) fn with_defaults() -> Self {
        let mut km = Self::new();
        let normal: &[(&str, &[&str])] = &[
            ("h", &["move_left"]),
            ("j", &["move_down"]),
            ("k", &["move_up"]),
            ("l", &["move_right"]),
            ("0", &["move_line_start"]),
            ("$", &["move_line_end"]),
            ("w", &["move_word_forward"]),
            ("b", &["move_word_backward"]),
            ("gg", &["goto_first_line"]),
            ("K", &["hover"]),
            ("gd", &["goto_definition"]),
            ("gr", &["goto_references"]),
            ("]d", &["next_diagnostic"]),
            ("[d", &["previous_diagnostic"]),
            ("gt", &["tab_next"]),
            ("gT", &["tab_previous"]),
            ("<C-w>w", &["window_next"]),
            ("<C-w>h", &["window_left"]),
            ("<C-w>j", &["window_down"]),
            ("<C-w>k", &["window_up"]),
            ("<C-w>l", &["window_right"]),
            ("<C-w>s", &["split_rows"]),
            ("<C-w>v", &["split_columns"]),
            ("<C-w>c", &["window_close"]),
            ("<C-w>=", &["equalize_windows"]),
            ("<C-w>+", &["resize"]),
            ("<C-w>-", &["resize_vertical"]),
            ("<C-w>_", &["resize_vertical"]),
            ("G", &["goto_last_line"]),
            ("gf", &["goto_file"]),
            ("J", &["join"]),
            ("gJ", &["join_tight"]),
            ("q:", &["command_history"]),
            ("q/", &["search_history"]),
            ("<C-v>", &["visual_block"]),
            ("H", &["goto_screen_top"]),
            ("M", &["goto_screen_middle"]),
            ("L", &["goto_screen_bottom"]),
            ("<C-e>", &["scroll_line_down"]),
            ("<C-y>", &["scroll_line_up"]),
            ("<C-d>", &["half_page_down"]),
            ("<C-u>", &["half_page_up"]),
            ("<C-f>", &["page_down"]),
            ("<C-b>", &["page_up"]),
            ("i", &["enter_insert"]),
            ("I", &["enter_insert_before"]),
            ("a", &["append"]),
            ("A", &["append_eol"]),
            ("o", &["open_line_below"]),
            ("O", &["open_line_above"]),
            ("v", &["enter_visual"]),
            ("V", &["enter_visual_line"]),
            ("<CR>", &["open_place"]),
            ("=", &["reindent_motion"]),
            ("<GT>", &["indent_motion"]),
            ("<LT>", &["outdent_motion"]),
            ("gv", &["reselect"]),
            ("gi", &["insert_again"]),
            ("gq", &["format_motion"]),
            ("g~", &["toggle_case_motion"]),
            ("gu", &["lower_motion"]),
            ("gU", &["upper_motion"]),
            ("x", &["delete_char"]),
            ("d", &["delete_motion"]),
            ("y", &["yank_motion"]),
            ("c", &["change_motion"]),
            ("p", &["put_after"]),
            ("P", &["put_before"]),
            ("<C-c>", &["copy"]),
            ("<Del>", &["delete_char"]),
            ("~", &["toggle_case"]),
            ("%", &["match_bracket"]),
            ("<C-a>", &["increment"]),
            ("<C-x>", &["decrement"]),
            ("g;", &["change_older"]),
            ("g,", &["change_newer"]),
            ("<C-o>", &["jump_oldest"]),
            // A terminal cannot tell `<C-i>` from Tab: they are one byte, and
            // crossterm hands it over as a Tab — which is what vim binds to the
            // same thing in normal mode.
            ("<C-i>", &["jump_newest"]),
            ("<Tab>", &["jump_newest"]),
            ("u", &["undo"]),
            (".", &["repeat"]),
            ("<C-r>", &["redo"]),
            (":", &["enter_command"]),
            ("/", &["search_forward"]),
            ("?", &["search_backward"]),
            ("zf", &["fold_motion"]),
            ("zo", &["fold_open"]),
            ("zc", &["fold_close"]),
            ("zR", &["fold_open_all"]),
            ("zM", &["fold_close_all"]),
            ("zd", &["fold_delete"]),
            ("zm", &["fold_level_less"]),
            ("zr", &["fold_level_more"]),
            ("zx", &["fold_update"]),
            ("zj", &["fold_next"]),
            ("zk", &["fold_previous"]),
            ("zt", &["scroll_to_top"]),
            ("zz", &["scroll_to_center"]),
            ("zb", &["scroll_to_bottom"]),
            (";", &["find_repeat"]),
            (",", &["find_reverse"]),
            ("*", &["search_word_forward"]),
            ("#", &["search_word_backward"]),
            ("n", &["search_next"]),
            ("N", &["search_previous"]),
            ("<Esc>", &["escape"]),
            ("<Left>", &["move_left"]),
            ("<Right>", &["move_right"]),
            ("<Up>", &["move_up"]),
            ("<Down>", &["move_down"]),
            ("<Home>", &["move_line_start"]),
            ("<End>", &["move_line_end"]),
            ("<PageUp>", &["page_up"]),
            ("<PageDown>", &["page_down"]),
        ];
        for (lhs, commands) in normal {
            km.insert(
                Mode::Normal,
                lhs,
                commands.iter().map(|c| c.to_string()).collect(),
            )
            .expect("built-in bindings are valid");
        }

        // Insert mode must not swallow the navigation keys: a user typing text
        // still expects to be able to move around. These are ordinary bindings,
        // so `ed.map("i", ...)` overrides them like any other.
        let insert: &[(&str, &[&str])] = &[
            ("<Left>", &["move_left"]),
            ("<Right>", &["move_right"]),
            ("<Up>", &["move_up"]),
            ("<Down>", &["move_down"]),
            ("<Home>", &["move_line_start"]),
            ("<End>", &["move_line_end"]),
            ("<PageUp>", &["page_up"]),
            ("<PageDown>", &["page_down"]),
            // Insert mode's own delete: it must not take the register with it.
            ("<Del>", &["delete_forwards"]),
            ("<C-k>", &["signature_help"]),
            ("<Tab>", &["snippet_next"]),
            ("<S-Tab>", &["snippet_previous"]),
            ("<C-n>", &["snippet_choice_next"]),
            ("<C-p>", &["snippet_choice_previous"]),
            ("<C-y>", &["complete_accept"]),
            ("<C-e>", &["complete_cancel"]),
        ];
        for (lhs, commands) in insert {
            km.insert(
                Mode::Insert,
                lhs,
                commands.iter().map(|c| c.to_string()).collect(),
            )
            .expect("built-in bindings are valid");
        }

        // Visual mode binds what acts on the selection, and nothing else: every
        // key it does not bind falls through to the normal mode's motions, so
        // `v` + `w` + `d` works without a second copy of the keymap. `V` is not
        // here — it is bound in normal mode, which visual mode also reaches.
        let visual: &[(&str, &[&str])] = &[
            ("v", &["enter_visual"]),
            ("J", &["join_visual"]),
            ("gJ", &["join_tight_visual"]),
            ("$", &["block_to_end"]),
            ("I", &["insert_block_left"]),
            ("A", &["insert_block_right"]),
            ("<Esc>", &["escape_visual"]),
            ("d", &["delete_visual"]),
            ("x", &["delete_visual"]),
            ("y", &["yank_visual"]),
            ("c", &["change_visual"]),
            ("o", &["swap_ends"]),
            ("<C-c>", &["copy"]),
            ("p", &["put_visual"]),
            ("P", &["put_visual"]),
            ("<gt>", &["indent_visual"]),
            ("<lt>", &["outdent_visual"]),
            ("=", &["reindent_visual"]),
            ("u", &["lower_visual"]),
            ("U", &["upper_visual"]),
            ("~", &["toggle_case_visual"]),
            // Vim's case operators take the selection too, spelled both ways.
            ("gu", &["lower_visual"]),
            ("gU", &["upper_visual"]),
            ("g~", &["toggle_case_visual"]),
        ];
        for (lhs, commands) in visual {
            km.insert(
                Mode::Visual,
                lhs,
                commands.iter().map(|c| c.to_string()).collect(),
            )
            .expect("built-in bindings are valid");
        }
        km
    }

    /// Bind `lhs` (a key sequence) in `mode`, replacing any existing binding.
    ///
    /// An empty command list removes the binding, which is what `ed.map(..., nil)`
    /// means. A sequence binds in any mode, insert mode included: the editor
    /// holds the keys back until it knows whether they are a mapping, and types
    /// them after all when they are not.
    ///
    /// Returns an error when `lhs` is not valid key notation.
    pub(crate) fn bind(
        &mut self,
        mode: Mode,
        lhs: &str,
        commands: Vec<String>,
    ) -> Result<(), KeyParseError> {
        let keys = parse_key(lhs)?;
        let seq = encode_sequence(&keys);
        if commands.is_empty() {
            self.maps.remove(&(mode, seq));
        } else {
            self.maps.insert((mode, seq), Binding::new(commands));
        }
        Ok(())
    }

    fn insert(
        &mut self,
        mode: Mode,
        lhs: &str,
        commands: Vec<String>,
    ) -> Result<(), KeyParseError> {
        self.bind(mode, lhs, commands)
    }

    /// Resolve a sequence of already-encoded keys.
    pub fn lookup(&self, mode: Mode, sequence: &str) -> Lookup {
        if let Some(binding) = self.maps.get(&(mode, sequence.to_string())) {
            return Lookup::Exact(binding.clone());
        }
        let prefix = format!("{sequence}{SEQ_SEP}");
        if self
            .maps
            .keys()
            .any(|(m, k)| *m == mode && k.starts_with(&prefix))
        {
            return Lookup::Prefix;
        }
        Lookup::None
    }

    /// Every binding, for introspection and tests.
    /// Only the README-table test asks for this: the map is otherwise a lookup
    /// table, not a list.
    #[cfg(test)]
    pub(crate) fn bindings(&self) -> Vec<(Mode, String, Vec<String>)> {
        let mut out: Vec<_> = self
            .maps
            .iter()
            .map(|((m, k), b)| (*m, k.clone(), b.commands.clone()))
            .collect();
        out.sort_by(|a, b| a.0.name().cmp(b.0.name()).then(a.1.cmp(&b.1)));
        out
    }
}

/// Parse a key notation string into the keys it denotes.
/// The keys a `:normal` argument stands for.
///
/// The notation everything else uses — `<Esc>`, `<CR>`, `<C-w>` — is read here
/// too, because a person writing a `:` command has no way to type a control key.
/// What is *not* read as notation is anything that is not one of those names: a
/// `<` that does not open a known key, or a `>` with nothing before it, is the
/// character it looks like. That is the difference between `:normal A <` (two
/// characters) and `:normal A <Esc>` (a character and a key), and it is why this
/// is not simply [`parse_key`], which fails on the first stray angle bracket.
pub(crate) fn parse_normal_keys(argument: &str) -> Vec<KeyMsg> {
    let mut keys = Vec::new();
    let mut chars = argument.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '<' {
            keys.push(KeyMsg::new(KeyCode::Char(c)));
            continue;
        }
        // A `<` is notation only when a name and a `>` follow it within reason.
        let mut token = String::new();
        let mut rest = chars.clone();
        let closed = loop {
            match rest.next() {
                Some('>') => break true,
                Some(next) if token.len() < 12 => token.push(next),
                Some(_) => break false,
                None => break false,
            }
        };
        match if closed {
            parse_token(&token).ok()
        } else {
            None
        } {
            Some(key) => {
                // Take the characters the token used, and the `>`.
                for _ in 0..token.chars().count() + 1 {
                    chars.next();
                }
                keys.push(key);
            }
            None => keys.push(KeyMsg::new(KeyCode::Char('<'))),
        }
    }
    keys
}

/// A sequence of keys in the notation the keymap is written in: `gg`, `d i w`,
/// `<Esc>`, `<C-w>v`. Every name in angle brackets is a key; anything else is one
/// character per key. A name that is not one of them is an error here, which is
/// what makes this the wrong reader for a `:` line — see
/// `parse_normal_keys` for the one that takes a person's word for it.
pub fn parse_key(s: &str) -> Result<Vec<KeyMsg>, KeyParseError> {
    let mut keys = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '<' {
            if c == '>' {
                return Err(KeyParseError(format!("stray '>' in {s:?}")));
            }
            keys.push(KeyMsg::new(KeyCode::Char(c)));
            continue;
        }
        let mut token = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == '>' {
                closed = true;
                break;
            }
            token.push(c);
        }
        if !closed {
            return Err(KeyParseError(format!("unterminated '<' in {s:?}")));
        }
        if token.is_empty() {
            return Err(KeyParseError(format!("empty <> in {s:?}")));
        }
        keys.push(parse_token(&token)?);
    }
    if keys.is_empty() {
        return Err(KeyParseError("empty key sequence".to_string()));
    }
    Ok(keys)
}

fn parse_token(token: &str) -> Result<KeyMsg, KeyParseError> {
    let (mods, name) = match token.rsplit_once('-') {
        Some((prefix, last)) if !prefix.is_empty() && !last.is_empty() => {
            let mut mods = Vec::new();
            for part in prefix.split('-') {
                match part {
                    "C" | "c" => mods.push(Mod::Ctrl),
                    "A" | "a" => mods.push(Mod::Alt),
                    "S" | "s" => mods.push(Mod::Shift),
                    other => {
                        return Err(KeyParseError(format!("unknown modifier {other:?}")));
                    }
                }
            }
            (mods, last)
        }
        _ => (Vec::new(), token),
    };
    Ok(KeyMsg {
        code: decode_name(name)?,
        mods,
    })
}

fn decode_name(name: &str) -> Result<KeyCode, KeyParseError> {
    // The spellings a character uses inside angle notation, because a bare
    // character cannot express them.
    match name {
        "Space" => return Ok(KeyCode::Char(' ')),
        "lt" | "LT" => return Ok(KeyCode::Char('<')),
        "gt" | "GT" => return Ok(KeyCode::Char('>')),
        _ => {}
    }

    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(KeyCode::Char(c));
    }

    // Everything else comes from the table the protocol generates, so a new key
    // is nameable the moment it is declared.
    KeyCode::ALL
        .iter()
        .copied()
        .find(|code| code.spellings().is_some_and(|names| names.contains(&name)))
        .ok_or_else(|| KeyParseError(format!("unknown key name {name:?}")))
}

/// The notation for a key without modifiers.
///
/// A character is written as itself; every other key is written in angle
/// brackets, which is what makes `<C-Esc>` unambiguous.
fn bare_notation(code: KeyCode) -> String {
    match code {
        KeyCode::Char(c) => match c {
            ' ' => "<Space>".to_string(),
            '<' => "<lt>".to_string(),
            '>' => "<gt>".to_string(),
            other => other.to_string(),
        },
        other => match other.spellings().and_then(|names| names.first()) {
            Some(name) => format!("<{name}>"),
            None => unreachable!("{other:?} has no spelling: teach the notation layer about it"),
        },
    }
}

/// The canonical notation for one key.
pub fn encode_key(key: &KeyMsg) -> String {
    let name = bare_notation(key.code);
    if key.mods.is_empty() {
        return name;
    }
    let mut mods = String::new();
    for (modifier, prefix) in [(Mod::Ctrl, "C-"), (Mod::Alt, "A-"), (Mod::Shift, "S-")] {
        if key.mods.contains(&modifier) {
            mods.push_str(prefix);
        }
    }
    let inner = name
        .strip_prefix('<')
        .and_then(|n| n.strip_suffix('>'))
        .unwrap_or(&name);
    format!("<{mods}{inner}>")
}

/// The canonical notation for a sequence of keys, as used by the keymap keys.
pub(crate) fn encode_sequence(keys: &[KeyMsg]) -> String {
    keys.iter()
        .map(encode_key)
        .collect::<Vec<_>>()
        .join(&SEQ_SEP.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(s: &str) -> String {
        encode_sequence(&parse_key(s).unwrap())
    }

    #[test]
    fn plain_and_special_keys_parse() {
        assert_eq!(
            parse_key("h").unwrap(),
            vec![KeyMsg::new(KeyCode::Char('h'))]
        );
        assert_eq!(
            parse_key(":").unwrap(),
            vec![KeyMsg::new(KeyCode::Char(':'))]
        );
        assert_eq!(parse_key("<Esc>").unwrap(), vec![KeyMsg::new(KeyCode::Esc)]);
        assert_eq!(
            parse_key("<CR>").unwrap(),
            vec![KeyMsg::new(KeyCode::Enter)]
        );
        assert_eq!(
            parse_key("<C-w>").unwrap(),
            vec![KeyMsg {
                code: KeyCode::Char('w'),
                mods: vec![Mod::Ctrl]
            }]
        );
        assert_eq!(
            parse_key("<C-A-Left>").unwrap(),
            vec![KeyMsg {
                code: KeyCode::Left,
                mods: vec![Mod::Ctrl, Mod::Alt]
            }]
        );
    }

    #[test]
    fn multi_key_sequences_parse() {
        assert_eq!(
            parse_key("dd").unwrap(),
            vec![
                KeyMsg::new(KeyCode::Char('d')),
                KeyMsg::new(KeyCode::Char('d'))
            ]
        );
        assert_eq!(seq("dd"), "d d");
        assert_eq!(seq("gg"), "g g");
    }

    #[test]
    fn notation_is_total_and_round_trips() {
        for notation in [
            "a",
            "Z",
            "0",
            "$",
            "<Esc>",
            "<CR>",
            "<BS>",
            "<Tab>",
            "<Space>",
            "<lt>",
            "<gt>",
            "<Left>",
            "<Right>",
            "<Up>",
            "<Down>",
            "<Home>",
            "<End>",
            "<Del>",
            "<PageUp>",
            "<PageDown>",
            "<C-w>",
            "<A-x>",
            "<S-Tab>",
            "<C-A-Esc>",
        ] {
            let keys = parse_key(notation).unwrap_or_else(|e| panic!("{notation}: {e}"));
            let encoded = encode_sequence(&keys);
            let reparsed = parse_key(&encoded).unwrap();
            assert_eq!(keys, reparsed, "{notation} -> {encoded}");
        }
    }

    #[test]
    fn every_protocol_key_has_a_notation() {
        // Iterating `KeyCode::ALL` rather than a hand-written list is what makes
        // a new protocol key impossible to add without teaching the notation.
        for code in inkstone_rpc::KeyCode::ALL.iter().copied() {
            let key = KeyMsg::new(code);
            let encoded = encode_key(&key);
            let decoded = parse_key(&encoded).unwrap_or_else(|e| {
                panic!("{code:?} encodes to {encoded:?}, which does not parse: {e}")
            });
            assert_eq!(decoded, vec![key], "{code:?} does not survive {encoded:?}");
        }
    }

    #[test]
    fn malformed_notation_is_rejected() {
        assert!(parse_key("").is_err());
        assert!(parse_key("<Esc").is_err());
        assert!(parse_key("<>").is_err());
        assert!(parse_key("<Nope>").is_err());
        assert!(parse_key("<X-a>").is_err());
        assert!(parse_key(">").is_err());
    }

    /// The README's table is what a user reads; this makes it impossible for it
    /// to drift away from the bindings the editor actually ships.
    #[test]
    fn the_readme_keymap_table_matches_the_defaults() {
        let readme =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
                .expect("README.md should exist in the workspace root");
        let table = readme
            .split_once("<!-- keymap:begin -->")
            .and_then(|(_, rest)| rest.split_once("<!-- keymap:end -->"))
            .map(|(table, _)| table)
            .expect("README.md should contain the keymap block");

        let mut documented: Vec<(Mode, String, Vec<String>)> = Vec::new();
        for row in table.lines() {
            let row = row.trim();
            if !row.starts_with('|') {
                continue;
            }
            let columns: Vec<&str> = row.trim_matches('|').split('|').map(str::trim).collect();
            let [mode, keys, commands] = columns[..] else {
                continue;
            };
            let mode = match mode {
                "n" => Mode::Normal,
                "v" => Mode::Visual,
                "i" => Mode::Insert,
                // The header and the `| --- |` separator row.
                "mode" => continue,
                other if other.starts_with('-') => continue,
                other => panic!("unknown mode {other:?} in the README keymap table"),
            };
            documented.push((
                mode,
                keys.trim_matches('`').to_string(),
                commands
                    .split(',')
                    .map(|c| c.trim().trim_matches('`').to_string())
                    .collect(),
            ));
        }

        let mut actual = Keymap::with_defaults().bindings();
        documented.sort_by(|a, b| (a.0.name(), &a.1).cmp(&(b.0.name(), &b.1)));
        actual.sort_by(|a, b| (a.0.name(), &a.1).cmp(&(b.0.name(), &b.1)));
        assert_eq!(
            documented, actual,
            "the README keymap table and the built-in keymap disagree"
        );
    }

    #[test]
    fn navigation_keys_are_bound_in_both_modes() {
        let km = Keymap::with_defaults();
        for lhs in [
            "<Left>",
            "<Right>",
            "<Up>",
            "<Down>",
            "<Home>",
            "<End>",
            "<PageUp>",
            "<PageDown>",
            "<Del>",
        ] {
            assert!(
                matches!(km.lookup(Mode::Normal, lhs), Lookup::Exact(_)),
                "{lhs} is unbound in normal mode"
            );
            assert!(
                matches!(km.lookup(Mode::Insert, lhs), Lookup::Exact(_)),
                "{lhs} is unbound in insert mode"
            );
        }
        // Typing still wins for ordinary characters.
        assert_eq!(km.lookup(Mode::Insert, "h"), Lookup::None);
    }

    #[test]
    fn defaults_lookup_exactly_and_by_prefix() {
        let km = Keymap::with_defaults();
        assert_eq!(
            km.lookup(Mode::Normal, "h"),
            Lookup::Exact(Binding::new(vec!["move_left".into()]))
        );
        // `d` is a command of its own: it starts the delete operator and waits
        // for a motion, and `dd` is that operator applied to itself.
        assert_eq!(
            km.lookup(Mode::Normal, "d"),
            Lookup::Exact(Binding::new(vec!["delete_motion".into()]))
        );
        assert_eq!(km.lookup(Mode::Normal, "d d"), Lookup::None);
        assert_eq!(km.lookup(Mode::Normal, "g"), Lookup::Prefix);
        // `q` is a prefix too: `q:` and `q/` are the history windows.
        assert_eq!(km.lookup(Mode::Normal, "q"), Lookup::Prefix);
        assert_eq!(
            km.lookup(Mode::Normal, "q :"),
            Lookup::Exact(Binding::new(vec!["command_history".into()]))
        );
        assert_eq!(km.lookup(Mode::Normal, "<Space>"), Lookup::None);
        assert_eq!(km.lookup(Mode::Insert, "h"), Lookup::None);
        assert_eq!(km.lookup(Mode::Normal, "d z"), Lookup::None);
    }

    #[test]
    fn insert_mode_takes_sequences_like_any_other_mode() {
        let mut km = Keymap::with_defaults();
        km.bind(Mode::Insert, "jk", vec!["escape".into()]).unwrap();
        assert_eq!(
            km.lookup(Mode::Insert, "j"),
            Lookup::Prefix,
            "the first key of the sequence waits for the rest"
        );
        assert_eq!(
            km.lookup(Mode::Insert, "j k"),
            Lookup::Exact(Binding::new(vec!["escape".into()]))
        );
        assert_eq!(km.lookup(Mode::Insert, "x"), Lookup::None);
        km.bind(Mode::Normal, "jk", vec!["escape".into()]).unwrap();
    }

    #[test]
    fn binding_overrides_and_validates() {
        let mut km = Keymap::with_defaults();
        km.bind(Mode::Normal, "x", vec!["move_right".into()])
            .unwrap();
        assert_eq!(
            km.lookup(Mode::Normal, "x"),
            Lookup::Exact(Binding::new(vec!["move_right".into()]))
        );
        assert!(km
            .bind(Mode::Normal, "<Nope>", vec!["noop".into()])
            .is_err());

        km.bind(Mode::Insert, "<C-s>", vec!["save".into()]).unwrap();
        assert_eq!(
            km.lookup(Mode::Insert, "<C-s>"),
            Lookup::Exact(Binding::new(vec!["save".into()]))
        );
    }

    #[test]
    fn encoded_notation_is_used_as_the_key() {
        let mut km = Keymap::new();
        km.bind(Mode::Normal, "<Space>", vec!["noop".into()])
            .unwrap();
        // Both the literal notation and the encoded form resolve to the same entry.
        assert_eq!(
            km.lookup(Mode::Normal, "<Space>"),
            Lookup::Exact(Binding::new(vec!["noop".into()]))
        );
        assert!(km.bindings().iter().any(|(_, k, _)| k == "<Space>"));
    }
}
