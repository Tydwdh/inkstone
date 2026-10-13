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

use edit_rpc::{KeyCode, KeyMsg, Mod};

use crate::mode::Mode;

/// The separator between encoded keys in a sequence.
const SEQ_SEP: char = ' ';

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
    pub commands: Vec<String>,
}

impl Binding {
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
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in vim-like bindings.
    pub fn with_defaults() -> Self {
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
            ("G", &["goto_last_line"]),
            ("i", &["enter_insert"]),
            ("I", &["enter_insert_before"]),
            ("a", &["append"]),
            ("A", &["append_eol"]),
            ("o", &["open_line_below"]),
            ("O", &["open_line_above"]),
            ("x", &["delete_char"]),
            ("<Del>", &["delete_char"]),
            ("dd", &["delete_line"]),
            (":", &["enter_command"]),
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
            ("<Del>", &["delete_char"]),
        ];
        for (lhs, commands) in insert {
            km.insert(
                Mode::Insert,
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
    /// means.
    ///
    /// Returns an error when `lhs` is not valid key notation.
    pub fn bind(
        &mut self,
        mode: Mode,
        lhs: &str,
        commands: Vec<String>,
    ) -> Result<(), KeyParseError> {
        let keys = parse_key(lhs)?;
        // Insert mode resolves one key at a time (typing must stay immediate),
        // so a multi-key mapping there would silently never fire. Say so instead.
        if mode == Mode::Insert && keys.len() > 1 {
            return Err(KeyParseError(format!(
                "insert-mode mappings must be a single key, got {lhs:?}"
            )));
        }
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
    pub fn bindings(&self) -> Vec<(Mode, String, Vec<String>)> {
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
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(KeyCode::Char(c));
    }
    Ok(match name {
        "Esc" | "Escape" => KeyCode::Esc,
        "CR" | "Enter" | "Return" => KeyCode::Enter,
        "BS" | "Backspace" => KeyCode::Backspace,
        "Tab" => KeyCode::Tab,
        "Space" => KeyCode::Char(' '),
        "lt" | "LT" => KeyCode::Char('<'),
        "gt" | "GT" => KeyCode::Char('>'),
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "Del" | "Delete" => KeyCode::Delete,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        _ => return Err(KeyParseError(format!("unknown key name {name:?}"))),
    })
}

/// The canonical notation for one key.
pub fn encode_key(key: &KeyMsg) -> String {
    let name = match key.code {
        KeyCode::Char(' ') => "<Space>".to_string(),
        KeyCode::Char('<') => "<lt>".to_string(),
        KeyCode::Char('>') => "<gt>".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "<CR>".to_string(),
        KeyCode::Esc => "<Esc>".to_string(),
        KeyCode::Backspace => "<BS>".to_string(),
        KeyCode::Tab => "<Tab>".to_string(),
        KeyCode::Left => "<Left>".to_string(),
        KeyCode::Right => "<Right>".to_string(),
        KeyCode::Up => "<Up>".to_string(),
        KeyCode::Down => "<Down>".to_string(),
        KeyCode::Home => "<Home>".to_string(),
        KeyCode::End => "<End>".to_string(),
        KeyCode::Delete => "<Del>".to_string(),
        KeyCode::PageUp => "<PageUp>".to_string(),
        KeyCode::PageDown => "<PageDown>".to_string(),
    };
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
pub fn encode_sequence(keys: &[KeyMsg]) -> String {
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
        for code in edit_rpc::KeyCode::ALL {
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
        assert_eq!(km.lookup(Mode::Normal, "d"), Lookup::Prefix);
        assert_eq!(
            km.lookup(Mode::Normal, "d d"),
            Lookup::Exact(Binding::new(vec!["delete_line".into()]))
        );
        assert_eq!(km.lookup(Mode::Normal, "g"), Lookup::Prefix);
        assert_eq!(km.lookup(Mode::Normal, "z"), Lookup::None);
        assert_eq!(km.lookup(Mode::Insert, "h"), Lookup::None);
        assert_eq!(km.lookup(Mode::Normal, "d z"), Lookup::None);
    }

    #[test]
    fn insert_mode_rejects_multi_key_mappings() {
        // `ed.map("i", "jk", "<Esc>")` cannot work yet: it must not be accepted
        // and then quietly ignored.
        let mut km = Keymap::with_defaults();
        let err = km
            .bind(Mode::Insert, "jk", vec!["escape".into()])
            .unwrap_err();
        assert!(err.to_string().contains("single key"), "{err}");
        assert_eq!(km.lookup(Mode::Insert, "j"), Lookup::None);
        // A single key is fine, and normal mode still takes sequences.
        km.bind(Mode::Insert, "j", vec!["escape".into()]).unwrap();
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
