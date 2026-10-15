//! The `inkstone` daemon: owns the editor, speaks msgpack RPC over a byte stream.
//!
//! The daemon is deliberately thin. All the behaviour lives in [`inkstone_core`];
//! this crate is the seam that turns a stream of [`ClientMsg`]s into a stream
//! of [`ServerMsg`]s and nothing else. [`Session`] is that seam expressed as a
//! pure, blocking-free function so it can be driven from a test without
//! spawning a process or a thread.

#![warn(missing_docs)]

use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;

use crossbeam_channel::{unbounded, Sender, TryRecvError};

use anyhow::{Context, Result};
use inkstone_core::{Ask, CompletionItem, Diagnostic, Editor, Severity};
use inkstone_lsp::{position, uri_from_path, Client as LspClient};
use inkstone_rpc::{read_client_msg, write_server_msg, ClientMsg, Grid, ServerMsg};
use serde_json::Value;

mod job;
mod terminal;

use job::Job;
use terminal::Terminal;

/// How the daemon should find its Lua configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitConfig {
    /// Look for `init.lua` in the platform config directory.
    Default,
    /// Use this file.
    Path(PathBuf),
    /// Source nothing.
    None,
}

impl InitConfig {
    /// The file to source, if any.
    pub fn resolve(&self) -> Option<PathBuf> {
        match self {
            InitConfig::Default => Editor::default_init_path().filter(|p| p.exists()),
            InitConfig::Path(p) => Some(p.clone()),
            InitConfig::None => None,
        }
    }
}

/// The messages a handled client request produced.
#[derive(Debug)]
pub struct Response {
    /// Messages to send back to the frontend, in order.
    pub messages: Vec<ServerMsg>,
    /// Whether the session is over.
    pub quit: bool,
}

/// An editor plus the RPC session state around it.
pub struct Session {
    editor: Editor,
    /// The screen the frontend was last sent, so the next frame can carry only
    /// the rows that changed.
    screen: Option<Grid>,
    /// The language server, when one is running for the file that is open.
    lsp: Option<LspServer>,
    /// The file a language server was last started for, so that a file no server
    /// was found for is not looked for again and again.
    lsp_tried_for: Option<PathBuf>,
    /// The command that is running, if one is: a process, its pipes and the
    /// thread that waits for it. See `job.rs` for why none of it is in the
    /// kernel.
    running: Option<Job>,
    /// The program that is running on a pty, if one is: `:term`'s other half.
    terminal: Option<Terminal>,
}

/// A language server, and what the session has to remember about talking to it.
pub struct LspServer {
    client: LspClient,
    /// The request that is out: its id, and what was asked — a completion's
    /// answer goes in the popup, a hover's in the message row, a definition's
    /// somewhere else in the file.
    asking: Option<(i64, Ask)>,
    /// What the last completion answer held, so that one item of it can be asked
    /// about again: `completionItem/resolve` is about an item, and the item is
    /// what the server sent — `data` and all — rather than what the kernel made
    /// of it.
    pub raw_completions: Vec<Value>,
    /// The item that is out for resolving: its request id, and its label.
    resolving: Option<(i64, String)>,
    /// Which file the server was told about: it is told again when this changes.
    uri: String,
    /// The version the server has, which is how many edits the document had
    /// when it was last sent.
    version: u64,
    /// Whether the server has been told the file is open at all.
    opened: bool,
}

/// The command that starts a language server for a file of this kind, if there
/// is one to start. The first word is looked for on `PATH`; the environment can
/// name a command instead, which is how a server that is not installed under its
/// usual name — or a fixture — is tried.
fn server_command(path: Option<&std::path::Path>) -> Option<String> {
    if let Ok(command) = std::env::var("INKSTONE_LSP") {
        if !command.trim().is_empty() {
            return Some(command);
        }
    }
    let extension = path
        .and_then(|path| path.extension())
        .and_then(|e| e.to_str())?;
    let candidates: &[&str] = match extension {
        "rs" => &["rust-analyzer"],
        "py" | "pyi" => &["pyright-langserver --stdio", "pylsp"],
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" => &["typescript-language-server --stdio"],
        "go" => &["gopls"],
        _ => &[],
    };
    candidates
        .iter()
        .find(|command| on_path(command))
        .map(|command| command.to_string())
}

/// Whether the program a command starts with is on `PATH`.
/// The items a completion response offers, in the kernel's terms.
///
/// A real server answers with either a list of items or an object with a list in
/// it; an item may say what to type in `insertText`, or in a `TextEdit` whose
/// new text is the thing to insert, or in neither, in which case its label is
/// what goes in. All three are the same answer to a person.
pub fn completion_items_from(answer: &Value) -> Vec<CompletionItem> {
    let list = answer
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| answer.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for item in list {
        let Some(label) = item["label"].as_str() else {
            continue;
        };
        let insert = item["insertText"]
            .as_str()
            .or_else(|| item["textEdit"]["newText"].as_str())
            .unwrap_or(label)
            .to_string();
        out.push(CompletionItem {
            label: label.to_string(),
            insert,
            detail: item["detail"].as_str().unwrap_or("").to_string(),
            // `insertTextFormat` 2 is a snippet: the text has placeholders in
            // it, and stepping through them is the kernel's business.
            snippet: item["insertTextFormat"].as_u64() == Some(2),
        });
    }
    out
}

/// What a hover says, as one line.
///
/// The protocol allows a string, a marked string with a language, a list of
/// either, or a `MarkupContent` with a plaintext or markdown body. All of them
/// are the same thing to a person reading a status line, so all of them come out
/// as text.
pub fn hover_text_from(answer: &Value) -> String {
    fn one(value: &Value) -> Option<String> {
        if let Some(text) = value.as_str() {
            return Some(text.to_string());
        }
        for key in ["value", "contents"] {
            if let Some(text) = value.get(key).and_then(Value::as_str) {
                return Some(text.to_string());
            }
        }
        None
    }
    let contents = &answer["contents"];
    let text = match contents {
        Value::Array(list) => list
            .iter()
            .filter_map(one)
            .collect::<Vec<String>>()
            .join("  "),
        other => one(other).unwrap_or_default(),
    };
    let text = text.trim();
    if text.is_empty() {
        "nothing to say about this".to_string()
    } else {
        // A status line is one line: the rest of a markdown body is not read
        // there, and the frontends would cut it anyway.
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<&str>>()
            .join(" ")
    }
}

/// Where the places in an answer are, in this editor's terms.
///
/// The protocol answers a definition with one location, a list of them, or a
/// `LocationLink` with the same information under different names; a list of
/// references is a list of locations. All of them come out as the same thing: a
/// file, a line and a column, which is what this editor can act on.
pub fn locations_from(answer: &Value) -> Vec<(String, usize, usize)> {
    let list: Vec<Value> = match answer {
        Value::Array(list) => list.clone(),
        other => vec![other.clone()],
    };
    list.iter()
        .filter_map(|place| {
            // A place may carry its own position or hide it in a `location`,
            // which is what a `SymbolInformation` does.
            let source = if place["location"].is_object() {
                &place["location"]
            } else {
                place
            };
            let uri = source["uri"]
                .as_str()
                .or_else(|| source["targetUri"].as_str())?
                .to_string();
            let range = if source["range"].is_null() {
                source["targetRange"].clone()
            } else {
                source["range"].clone()
            };
            let row = range["start"]["line"].as_u64().unwrap_or(0) as usize;
            let character = range["start"]["character"].as_u64().unwrap_or(0) as usize;
            Some((uri, row, character))
        })
        .collect()
}

/// The first place in an answer, which is where a person asked to go.
pub fn definition_from(answer: &Value) -> Option<(String, usize, usize)> {
    locations_from(answer).into_iter().next()
}

/// What a call takes, as one line.
///
/// The answer holds one signature per way the call can be made, and which of them
/// is being typed — a server may also say which parameter is current, either by
/// index into the label's parts or by naming it. The label alone is what the
/// message row has room for, with the current parameter's own text beside it when
/// it can be found, since that is the part a person is actually typing.
pub fn signature_text(answer: &Value) -> String {
    let signatures = answer["signatures"].as_array().cloned().unwrap_or_default();
    if signatures.is_empty() {
        return "no signature".to_string();
    }
    let active = answer["activeSignature"].as_u64().unwrap_or(0) as usize;
    let signature = signatures
        .get(active)
        .or_else(|| signatures.first())
        .expect("not empty");
    let label = signature["label"].as_str().unwrap_or("").trim();
    if label.is_empty() {
        return "no signature".to_string();
    }
    let parameter = answer["activeParameter"]
        .as_u64()
        .zip(signature["parameters"].as_array())
        .and_then(|(index, list)| list.get(index as usize).cloned())
        .and_then(|parameter| {
            // A parameter's label is a string, or a `[start, end]` pair of
            // character offsets into the signature's own label.
            if let Some(text) = parameter["label"].as_str() {
                return Some(text.trim().to_string());
            }
            let start = parameter["label"][0].as_u64()? as usize;
            let end = parameter["label"][1].as_u64()? as usize;
            let chars: Vec<char> = label.chars().collect();
            (start < end && end <= chars.len())
                .then(|| chars[start..end].iter().collect::<String>())
        })
        .unwrap_or_default();
    if parameter.is_empty() || !label.contains(&parameter) {
        label.to_string()
    } else {
        format!("{label}   ({} now)", parameter.trim())
    }
}

/// What a file holds, as one line.
///
/// A server answers with a list of symbols — either nested ones that carry their
/// own positions, or flat ones whose position is inside a `location`. Both read
/// as the same thing here: a name, what kind of thing it is, and the line it is
/// on, which is what the message row can hold and what a person is looking for.
pub fn symbols_text(answer: &Value) -> String {
    let symbols = symbols_from(answer);
    if symbols.is_empty() {
        return "no symbols".to_string();
    }
    let mut parts = vec![format!(
        "{} symbol{}",
        symbols.len(),
        if symbols.len() == 1 { "" } else { "s" }
    )];
    for (name, kind, row) in symbols.iter().take(8) {
        parts.push(format!("{name} ({kind}) on line {}", row + 1));
    }
    if symbols.len() > 8 {
        parts.push(format!("... {} more", symbols.len() - 8));
    }
    parts.join("   ")
}

/// The symbols in an answer: a name, what kind of thing it is, and its line.
///
/// `SymbolKind` is a number in the protocol; the words here are the ones a person
/// would use for the same thing, and an unknown number is left as a number rather
/// than guessed at.
pub fn symbols_from(answer: &Value) -> Vec<(String, String, usize)> {
    fn kind_of(value: &Value) -> String {
        let kind = value["kind"].as_u64().unwrap_or(0);
        match kind {
            1 => "file".to_string(),
            2 => "module".to_string(),
            3 => "namespace".to_string(),
            4 => "package".to_string(),
            5 => "class".to_string(),
            6 => "method".to_string(),
            7 => "property".to_string(),
            8 => "field".to_string(),
            9 => "constructor".to_string(),
            10 => "enum".to_string(),
            11 => "interface".to_string(),
            12 => "function".to_string(),
            13 => "variable".to_string(),
            14 => "constant".to_string(),
            15 => "string".to_string(),
            16 => "number".to_string(),
            17 => "boolean".to_string(),
            18 => "array".to_string(),
            19 => "object".to_string(),
            20 => "key".to_string(),
            21 => "null".to_string(),
            22 => "enum member".to_string(),
            23 => "struct".to_string(),
            24 => "event".to_string(),
            25 => "operator".to_string(),
            26 => "type parameter".to_string(),
            other => other.to_string(),
        }
    }
    fn collect(value: &Value, out: &mut Vec<(String, String, usize)>) {
        if let Some(list) = value.as_array() {
            for item in list {
                collect(item, out);
            }
            return;
        }
        let Some(name) = value["name"].as_str() else {
            return;
        };
        let range = if value["location"]["range"].is_null() {
            value["range"].clone()
        } else {
            value["location"]["range"].clone()
        };
        let row = range["start"]["line"].as_u64().unwrap_or(0) as usize;
        out.push((name.to_string(), kind_of(value), row));
        // Nested symbols — a class and its methods — are symbols too.
        collect(&value["children"], out);
    }
    let mut out = Vec::new();
    collect(answer, &mut out);
    out
}

/// The edits an answer asks for, in this editor's terms: character offsets into
/// the text as it is now.
///
/// A server's edit is a range in UTF-16 code units and a new text, and both are
/// converted here — the columns against the line they are on, which is the same
/// conversion every position from a server goes through.
pub fn edits_from(answer: &Value, buffer: &inkstone_core::Buffer) -> Vec<(usize, usize, String)> {
    let Some(list) = answer.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|edit| {
            let range = &edit["range"];
            let row = range["start"]["line"].as_u64()? as usize;
            let end_row = range["end"]["line"].as_u64()? as usize;
            let start_line = buffer.line_string(row.min(buffer.line_count().saturating_sub(1)));
            let end_line = buffer.line_string(end_row.min(buffer.line_count().saturating_sub(1)));
            let start_col = position::utf16_to_char(
                &start_line,
                range["start"]["character"].as_u64().unwrap_or(0) as usize,
            );
            let end_col = position::utf16_to_char(
                &end_line,
                range["end"]["character"].as_u64().unwrap_or(0) as usize,
            );
            let start = buffer.line_col_to_char(row, start_col);
            let end = buffer.line_col_to_char(end_row, end_col);
            let text = edit["newText"].as_str().unwrap_or("").to_string();
            Some((start, end, text))
        })
        .collect()
}

/// The word the cursor is in the middle of, for a query.
fn word_at(line: &str, col: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut start = col.min(chars.len());
    while start > 0 && chars.get(start - 1).is_some_and(|c| is_word(*c)) {
        start -= 1;
    }
    let mut end = col.min(chars.len());
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    chars[start..end].iter().collect()
}

/// The places an answer names, as a file and a position each.
pub fn places_from(answer: &Value) -> Vec<(std::path::PathBuf, usize, usize)> {
    locations_from(answer)
        .into_iter()
        .filter_map(|(uri, row, col)| path_from_uri(&uri).map(|path| (path, row, col)))
        .collect()
}

/// Go to the first place a reference answer names, if there is one.
///
/// The list is what the message row can hold; the first place is where a person
/// meant to go, and coming back from it is what the jump list is for.
pub fn goto_first_reference(editor: &mut inkstone_core::Editor, answer: &Value) {
    let Some((uri, row, character)) = locations_from(answer).into_iter().next() else {
        return;
    };
    let Some(path) = path_from_uri(&uri) else {
        return;
    };
    // The column is in UTF-16 code units and needs the line's text, which may be
    // in a file this session has not opened yet.
    let line = if path == editor.buffer.path().unwrap_or(&path) {
        editor.buffer.line_string(row)
    } else {
        inkstone_core::Buffer::from_path(&path)
            .map(|buffer| buffer.line_string(row.min(buffer.line_count().saturating_sub(1))))
            .unwrap_or_default()
    };
    let col = position::utf16_to_char(&line, character);
    let _ = editor.goto_location(&path, row, col);
}

/// Where a name is used, as one line.
///
/// There is one line of message row and no list to put references in, so this is
/// what `gr` can be here: how many there are, and where the first few are, named
/// by the file they are in — the same shape `:diagnostics` and `:jumps` have.
pub fn references_text(answer: &Value) -> String {
    let places = locations_from(answer);
    if places.is_empty() {
        return "no references".to_string();
    }
    let mut parts = vec![format!(
        "{} reference{}",
        places.len(),
        if places.len() == 1 { "" } else { "s" }
    )];
    for (uri, row, character) in places.iter().take(8) {
        let name = path_from_uri(uri)
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| uri.clone());
        parts.push(format!("{name}:{}:{}", row + 1, character + 1));
    }
    if places.len() > 8 {
        parts.push(format!("... {} more", places.len() - 8));
    }
    parts.join("   ")
}

/// The path a `file://` URI names, on this platform.
pub fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    // A Windows URI is `/C:/a/b`: the leading slash is a URI's, not a path's.
    let path = if rest.len() > 2 && rest.as_bytes()[1] == b':' {
        rest.to_string()
    } else {
        format!("/{rest}")
    };
    Some(PathBuf::from(
        path.replace('/', std::path::MAIN_SEPARATOR_STR),
    ))
}

/// The language identifier the protocol wants for a file.
fn language_id(path: &std::path::Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust",
        Some("py") | Some("pyi") => "python",
        Some("js") | Some("mjs") | Some("cjs") => "javascript",
        Some("jsx") => "javascriptreact",
        Some("ts") => "typescript",
        Some("tsx") => "typescriptreact",
        Some("go") => "go",
        Some("c") | Some("h") => "c",
        Some("cc") | Some("cpp") | Some("cxx") | Some("hpp") => "cpp",
        Some("java") => "java",
        Some("json") => "json",
        Some(other) => other,
        None => "plaintext",
    }
    .to_string()
}

fn on_path(command: &str) -> bool {
    let Some(program) = command.split_whitespace().next() else {
        return false;
    };
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| {
        let candidate = directory.join(program);
        candidate.is_file()
            || candidate
                .with_extension(std::env::consts::EXE_EXTENSION)
                .is_file()
    })
}

/// What a language server says about a file, in the kernel's terms.
///
/// The protocol counts a column in UTF-16 code units and this editor counts
/// characters, so every position is converted against the text it points into —
/// which is why this takes the lines rather than the whole document: a payload
/// with a position on a line nobody asked for is clamped rather than believed.
pub fn diagnostics_from(
    params: &Value,
    line: impl Fn(usize) -> String,
    lines: usize,
) -> Vec<Diagnostic> {
    let Some(list) = params.get("diagnostics").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for diagnostic in list {
        let range = &diagnostic["range"];
        let number = |value: &Value| value.as_u64().map(|n| n as usize).unwrap_or(0);
        let row = number(&range["start"]["line"]).min(lines.saturating_sub(1));
        let end_row = number(&range["end"]["line"]).min(lines.saturating_sub(1));
        let text = line(row);
        let end_text = if end_row == row {
            text.clone()
        } else {
            line(end_row)
        };
        let severity = match diagnostic["severity"].as_u64() {
            Some(1) => Severity::Error,
            Some(2) => Severity::Warning,
            Some(3) => Severity::Information,
            Some(4) => Severity::Hint,
            // The protocol leaves it open when a server says nothing, and a
            // problem with no severity is still a problem.
            _ => Severity::Warning,
        };
        out.push(Diagnostic::new(
            (
                row,
                position::utf16_to_char(&text, number(&range["start"]["character"])),
            ),
            (
                end_row,
                position::utf16_to_char(&end_text, number(&range["end"]["character"])),
            ),
            severity,
            diagnostic["message"].as_str().unwrap_or("").to_string(),
        ));
    }
    out
}

impl Session {
    /// Wrap an editor in a session.
    pub fn new(editor: Editor) -> Self {
        Self {
            editor,
            screen: None,
            lsp: None,
            lsp_tried_for: None,
            running: None,
            terminal: None,
        }
    }

    /// Start a language server for the file that is open, if the setting says
    /// so and there is one to start. Says what happened, so a session that is
    /// driven by a test can tell.
    pub fn start_lsp(&mut self) -> Option<String> {
        if !self.editor.options.borrow().lsp {
            return None;
        }
        let command = match server_command(self.editor.buffer.path()) {
            Some(command) => command,
            None => {
                self.editor.message = format!(
                    "no language server for {}",
                    self.editor.buffer.display_name()
                );
                return None;
            }
        };
        match LspClient::spawn(&command) {
            Ok(mut client) => {
                if let Err(error) = client.initialize(self.root_uri().as_deref()) {
                    self.editor.message = format!("lsp: {error}");
                    return None;
                }
                self.lsp = Some(LspServer {
                    client,
                    asking: None,
                    raw_completions: Vec::new(),
                    resolving: None,
                    uri: String::new(),
                    version: 0,
                    opened: false,
                });
                Some(command)
            }
            Err(error) => {
                self.editor.message = format!("lsp: {command}: {error}");
                None
            }
        }
    }

    /// The folder the server should think it is working in: the one the file is
    /// in, which is what a server uses to find the rest of a project.
    fn root_uri(&self) -> Option<String> {
        let path = self.editor.buffer.path()?;
        let folder = path.parent()?;
        Some(uri_from_path(&folder.to_string_lossy()))
    }

    /// Everything the server has said since the last look, put where the kernel
    /// keeps it.
    ///
    /// A payload names a file by URI, and a session can hold several: the one on
    /// screen, whose state is live in the editor, and the ones in their slots.
    /// Diagnostics go to the document they are about, not to the one being
    /// looked at.
    pub fn pump_lsp(&mut self) {
        self.apply_resolved();
        self.apply_asking();
        self.collect_diagnostics();
    }

    /// The answer to a `completionItem/resolve`, which is an item's details: they
    /// go where the popup shows them.
    fn apply_resolved(&mut self) {
        // An item resolved: its details go where the popup shows them.
        let resolving = self
            .lsp
            .as_ref()
            .and_then(|server| server.resolving.clone());
        if let Some((id, label)) = resolving {
            let answer = self
                .lsp
                .as_mut()
                .and_then(|server| server.client.answer(id));
            if let Some(answer) = answer {
                if let Some(server) = self.lsp.as_mut() {
                    server.resolving = None;
                }
                // `detail` is the one-line thing the popup has room for; a
                // server that only sent documentation gets its first line.
                let detail = answer["detail"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| {
                        answer["documentation"]
                            .as_str()
                            .map(str::to_string)
                            .or_else(|| {
                                answer["documentation"]["value"]
                                    .as_str()
                                    .map(str::to_string)
                            })
                    })
                    .map(|text| {
                        text.lines()
                            .map(str::trim)
                            .find(|line| !line.is_empty())
                            .unwrap_or("")
                            .to_string()
                    })
                    .unwrap_or_default();
                if !detail.is_empty() {
                    self.editor.set_completion_detail(&label, &detail);
                }
            }
        }
    }

    /// The answer to whatever request was out, if it has arrived.
    fn apply_asking(&mut self) {
        // The answer to whatever was asked, if one is out and has arrived.
        let asking = self.lsp.as_ref().and_then(|server| server.asking.clone());
        if let Some((id, ask)) = asking {
            let answer = self
                .lsp
                .as_mut()
                .and_then(|server| server.client.answer(id));
            if let Some(answer) = answer {
                if let Some(server) = self.lsp.as_mut() {
                    server.asking = None;
                }
                match ask {
                    Ask::Completion { .. } => self.completion_answer(&answer),
                    Ask::Hover { .. } => self.editor.message = hover_text_from(&answer),
                    Ask::Definition { .. } => self.goto_definition_from(&answer),
                    Ask::Signature { .. } => self.editor.message = signature_text(&answer),
                    Ask::Format { save, .. } => self.format_answer(&answer, save),
                    Ask::WorkspaceSymbols { .. } => self.workspace_symbols_answer(&answer),
                    Ask::Symbols { .. } => self.symbols_answer(&answer),
                    Ask::References { .. } => self.references_answer(&answer),
                }
            }
        }
    }

    /// Take in what the server has published: the diagnostics for the file that is
    /// open, and for any other document this session holds.
    fn collect_diagnostics(&mut self) {
        let Some(server) = self.lsp.as_mut() else {
            return;
        };
        let messages = server.client.poll();
        for message in messages {
            if message.method() != Some("textDocument/publishDiagnostics") {
                continue;
            }
            let Some(params) = message.params() else {
                continue;
            };
            let Some(uri) = params["uri"].as_str() else {
                continue;
            };
            let live = self
                .editor
                .buffer
                .path()
                .map(|path| uri_from_path(&path.to_string_lossy()));
            if live.as_deref() == Some(uri) {
                let lines = self.editor.buffer.line_count();
                let diagnostics =
                    diagnostics_from(params, |row| self.editor.buffer.line_string(row), lines);
                self.editor.set_diagnostics(diagnostics);
                continue;
            }
            if let Some(document) = self.editor.documents.iter_mut().find(|document| {
                document
                    .buffer
                    .path()
                    .map(|path| uri_from_path(&path.to_string_lossy()))
                    .as_deref()
                    == Some(uri)
            }) {
                let lines = document.buffer.line_count();
                let line = |row: usize| document.buffer.line_string(row);
                document.diagnostics = diagnostics_from(params, line, lines);
                document
                    .diagnostics
                    .sort_by_key(inkstone_core::diagnostic::order);
                self.editor.redraw = true;
            }
        }
    }

    /// What a completion answer does: keep what was sent, and show what it means.
    ///
    /// What was sent is kept as well as what it means, because the details of one
    /// item are a question about *it*: `completionItem/resolve` asks the server
    /// about the item it sent, `data` and all.
    fn completion_answer(&mut self, answer: &Value) {
        if let Some(server) = self.lsp.as_mut() {
            server.raw_completions = answer
                .get("items")
                .and_then(Value::as_array)
                .or_else(|| answer.as_array())
                .cloned()
                .unwrap_or_default();
        }
        let items = completion_items_from(answer);
        if !items.is_empty() {
            self.editor.set_completions(items);
        }
    }

    /// What a formatting answer does: the edits, and — for `formatonsave` — the
    /// write that asked for them.
    ///
    /// The write that asked happened already, so a server that says nothing costs
    /// a person nothing; this writes *again*, which is what makes the file and the
    /// buffer agree.
    fn format_answer(&mut self, answer: &Value, save: bool) {
        let edits = edits_from(answer, &self.editor.buffer);
        let count = self.editor.apply_edits(&edits);
        self.editor.message = if count == 0 {
            "already formatted".to_string()
        } else {
            format!("{count} edit{} applied", if count == 1 { "" } else { "s" })
        };
        if save {
            match self.editor.write_buffer(None) {
                Ok(path) => self.editor.message = format!("\"{}\" written", path.display()),
                Err(error) => self.editor.message = error.to_string(),
            }
        }
    }

    /// What a workspace-symbols answer does: a list of places in *other* files, so
    /// the list is the answer and the window that shows it is opened.
    fn workspace_symbols_answer(&mut self, answer: &Value) {
        let places = places_from(answer);
        let count = places.len();
        self.editor.set_places(places);
        if count > 0 {
            self.editor.open_places_list();
        }
        self.editor.message = format!("{count} symbol{}", if count == 1 { "" } else { "s" });
    }

    /// What a symbols answer does: the places it names, and the first one.
    fn symbols_answer(&mut self, answer: &Value) {
        self.editor.set_places(places_from(answer));
        // The answer names places like any other; the first is where a person
        // meant to go.
        self.editor.message = symbols_text(answer);
        goto_first_reference(&mut self.editor, answer);
    }

    /// What a references answer does: the list is kept as a list as well as said
    /// once — `gr` says where the places are, and `:cnext` walks them.
    fn references_answer(&mut self, answer: &Value) {
        self.editor.set_places(places_from(answer));
        self.editor.message = references_text(answer);
        goto_first_reference(&mut self.editor, answer);
    }

    /// Ask about formatting: the file, or the range a `:{range}format` named.
    ///
    /// No position and no options a server would understand: what is asked for
    /// is the whole document laid out, or part of it — `textDocument/rangeFormatting`
    /// is the same answer about part of the file.
    fn ask_format(&mut self, ask: &Ask, uri: &str) {
        let Ask::Format { from, to, .. } = ask else {
            return;
        };

        let options = serde_json::json!({
            "tabSize": self.editor.options.borrow().tabstop,
            "insertSpaces": false
        });
        // A range makes it `rangeFormatting`, which is the same answer about
        // part of the file; no range is the whole document.
        let (method, params) = match (from, to) {
            (Some(from), Some(to)) => {
                let last = self.editor.buffer.line_count().saturating_sub(1);
                let (from, to) = ((*from).min(last), (*to).min(last));
                let start_line = self.editor.buffer.line_string(from);
                let end_line = self.editor.buffer.line_string(to);
                let range = serde_json::json!({
                    "start": { "line": from, "character": 0 },
                    "end": { "line": to, "character": end_line.chars().count() }
                });
                let _ = start_line;
                (
                    "textDocument/rangeFormatting",
                    serde_json::json!({
                        "textDocument": { "uri": uri },
                        "range": range,
                        "options": options
                    }),
                )
            }
            _ => (
                "textDocument/formatting",
                serde_json::json!({
                    "textDocument": { "uri": uri },
                    "options": options
                }),
            ),
        };
        let Some(server) = self.lsp.as_mut() else {
            return;
        };
        match server.client.request(method, params) {
            Ok(id) => server.asking = Some((id, ask.clone())),
            Err(error) => self.editor.message = format!("lsp: {error}"),
        }
    }

    /// What each request is called, and what its answer means.
    fn ask_lsp(&mut self) {
        let Some(ask) = self.editor.take_lsp_ask() else {
            return;
        };
        let (row, col) = ask.position();
        let Some(path) = self.editor.buffer.path() else {
            // Nothing to ask about — a list window, say — and saying so is
            // better than leaving the question hanging on the message row.
            self.editor.message = "no file to ask about".to_string();
            return;
        };
        let uri = uri_from_path(&path.to_string_lossy());
        let line = self.editor.buffer.line_string(row);
        let character = position::char_to_utf16(&line, col);
        let Some(server) = self.lsp.as_mut() else {
            return;
        };
        if !server.opened || server.uri != uri {
            // Nothing is open to complete in yet. The file is sent first, and
            // the next keystroke will have something to ask about.
            return;
        }
        if matches!(ask, Ask::Format { .. }) {
            self.ask_format(&ask, &uri);
            return;
        }
        // `workspace/symbol` is asked about a *name* rather than a place: the
        // word under the cursor is the query, and a cursor that is not on a word
        // asks for everything the server will give.
        if let Ask::WorkspaceSymbols { query: named, .. } = &ask {
            // `:wsymbols {query}` asks for what it names; without one, the word
            // under the cursor is the query.
            let query = match named {
                Some(named) if !named.is_empty() => named.clone(),
                _ => word_at(&line, col),
            };
            return match server
                .client
                .request("workspace/symbol", serde_json::json!({ "query": query }))
            {
                Ok(id) => server.asking = Some((id, ask)),
                Err(error) => self.editor.message = format!("lsp: {error}"),
            };
        }
        let method = match ask {
            Ask::Completion { .. } => "textDocument/completion",
            Ask::Hover { .. } => "textDocument/hover",
            Ask::Definition { .. } => "textDocument/definition",
            Ask::References { .. } => "textDocument/references",
            Ask::Symbols { .. } => "textDocument/documentSymbol",
            Ask::Signature { .. } => "textDocument/signatureHelp",
            Ask::WorkspaceSymbols { .. } => "workspace/symbol",
            Ask::Format { .. } => "textDocument/formatting",
        };
        match server.client.request(
            method,
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": { "line": row, "character": character }
            }),
        ) {
            Ok(id) => server.asking = Some((id, ask)),
            Err(error) => self.editor.message = format!("lsp: {error}"),
        }
    }

    /// Go where a definition answer points: the file, and a position in it,
    /// with the protocol's UTF-16 column converted against the line it names.
    fn goto_definition_from(&mut self, answer: &Value) {
        let Some((uri, row, character)) = definition_from(answer) else {
            self.editor.message = "no definition found".to_string();
            return;
        };
        let Some(path) = path_from_uri(&uri) else {
            self.editor.message = format!("cannot open {uri}");
            return;
        };
        // The column is in UTF-16 code units and needs the line's text, which
        // may be in a file this session has not opened yet.
        let line = if path == self.editor.buffer.path().unwrap_or(&path) {
            self.editor.buffer.line_string(row)
        } else {
            inkstone_core::Buffer::from_path(&path)
                .map(|buffer| buffer.line_string(row.min(buffer.line_count().saturating_sub(1))))
                .unwrap_or_default()
        };
        let col = position::utf16_to_char(&line, character);
        match self.editor.goto_location(&path, row, col) {
            Ok(()) => {}
            Err(error) => self.editor.message = error.to_string(),
        }
    }

    /// Whether a completion request is out.
    pub fn lsp_is_asking(&self) -> bool {
        self.lsp
            .as_ref()
            .is_some_and(|server| server.asking.is_some())
    }

    /// Tell the server what has happened to the file: that it is open, and what
    /// is in it when that changes.
    pub fn sync_lsp(&mut self) {
        if !self.editor.options.borrow().lsp {
            return;
        }
        let Some(path) = self.editor.buffer.path() else {
            return;
        };
        let uri = uri_from_path(&path.to_string_lossy());
        let edits = self.editor.edit_count();
        let language = language_id(path);
        let Some(server) = self.lsp.as_mut() else {
            return;
        };
        if !server.opened || server.uri != uri {
            if server.opened {
                let _ = server.client.did_close(&server.uri);
            }
            server.uri = uri.clone();
            server.version = edits;
            server.opened = true;
            let text = self.editor.buffer.text();
            let _ = server.client.did_open(&uri, &language, &text, edits as i64);
            return;
        }
        if server.version != edits {
            server.version = edits;
            let text = self.editor.buffer.text();
            let _ = server.client.did_change(&uri, &text, edits as i64);
        }
    }

    /// Stop the language server, if one is running.
    pub fn stop_lsp(&mut self) {
        if let Some(mut server) = self.lsp.take() {
            let _ = server.client.shutdown();
        }
    }

    /// Whether a language server is running for this session.
    pub fn lsp_is_running(&self) -> bool {
        self.lsp.is_some()
    }

    /// Look at what the server has said, and tell it what has changed about the
    /// file. This is what the daemon does when nothing else is happening: a
    /// server pushes when it likes, and a session that only looked at its
    /// messages while handling a key would hear a diagnostic a keystroke late.
    ///
    /// Everything this learns goes into the editor, so a message is produced the
    /// same way any other change to the editor produces one.
    /// Ask about the one item the list has moved to, if a list is up.
    ///
    /// This is the lazy half of completion: an answer names the items, and what
    /// each one *is* comes from asking about it — which is worth doing for the
    /// item a person is looking at and not for the ninety-nine they are not.
    fn ask_resolve(&mut self) {
        let Some(label) = self.editor.take_completion_resolve() else {
            return;
        };
        let Some(server) = self.lsp.as_mut() else {
            return;
        };
        if server.resolving.is_some() {
            return;
        }
        let Some(item) = server
            .raw_completions
            .iter()
            .find(|item| item["label"].as_str() == Some(label.as_str()))
        else {
            return;
        };
        match server
            .client
            .request("completionItem/resolve", item.clone())
        {
            Ok(id) => server.resolving = Some((id, label)),
            Err(error) => self.editor.message = format!("lsp: {error}"),
        }
    }

    /// One turn of the session's own work: the language server's answers, and
    /// the questions the editor has left for it.
    ///
    /// What an answer changes is the message row or a place on the screen, and
    /// it arrives between keystrokes — so what the frontend is shown is compared
    /// before and after, and a frame is asked for when it moved.
    pub fn tick(&mut self) {
        // What a language server's answer changes is the message row or a
        // place on the screen — and it arrives between keystrokes, so nothing
        // else would ask for a frame. What the frontend is shown is compared
        // before and after, and a frame is asked for when it moved.
        let before = (
            self.editor.message.clone(),
            self.editor.cursor.row,
            self.editor.cursor.col,
            self.editor.current_document(),
        );
        self.tick_lsp();
        self.tick_job();
        self.tick_terminal();
        self.ask_resolve();
        let after = (
            self.editor.message.clone(),
            self.editor.cursor.row,
            self.editor.cursor.col,
            self.editor.current_document(),
        );
        if before != after {
            self.editor.redraw = true;
        }
    }

    /// The command the editor asked for, and the answer it is waiting for.
    ///
    /// A command runs on its own threads and is *started* here, at a tick: that
    /// is what keeps a ten-second build from being ten seconds of a frozen
    /// editor, and it is the same shape the language server has.
    fn tick_job(&mut self) {
        // A script's own requests arrive without a key behind them — `init.lua`
        // calling `ed.shell` is the case — so the queue is drained here too, and
        // before the job slot is looked at: the request that starts a command is
        // often the one this turn's tick has to see.
        self.editor.run_pending_lua();
        if self.running.is_none() {
            if let Some(request) = self.editor.job_request() {
                match job::spawn(&request.command, request.input.as_deref()) {
                    Ok(running) => self.running = Some(running),
                    Err(error) => self.editor.job_failed(error.to_string()),
                }
            }
        }
        if self.editor.take_job_kill() {
            if let Some(running) = &self.running {
                running.kill();
            }
        }
        let Some(running) = &self.running else {
            return;
        };
        match running.outcome() {
            Ok(outcome) => {
                self.running = None;
                self.editor
                    .job_finished(outcome.output, outcome.stderr, outcome.status);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                // The thread that was going to answer is gone, which is the one
                // way a command can end without a result: say so rather than
                // leave the editor waiting for it forever.
                self.running = None;
                self.editor
                    .job_failed("it stopped without a word".to_string());
            }
        }
    }

    /// Stop the command that is running, if one is: the session is going away,
    /// and a process left behind would outlive the editor that asked for it.
    pub fn stop_job(&mut self) {
        if let Some(running) = &self.running {
            running.kill();
        }
        if let Some(terminal) = &mut self.terminal {
            terminal.kill();
        }
    }

    /// The pty behind `:term`: started, fed, written to, resized and stopped.
    ///
    /// Every one of those is a few bytes between the kernel and this, and none
    /// of them is on the wire: a terminal's screen is a buffer like any other,
    /// and a frontend draws it the way it draws everything else.
    fn tick_terminal(&mut self) {
        if self.terminal.is_none() {
            if let Some(program) = self.editor.take_terminal_request() {
                let (cols, rows) = self.editor.terminal_size();
                match Terminal::spawn(&program, cols, rows) {
                    Ok(terminal) => self.terminal = Some(terminal),
                    Err(error) => {
                        self.editor.terminal_exited(None);
                        self.editor.message = format!("E: cannot start a terminal: {error}");
                    }
                }
            }
        }
        let Some(terminal) = self.terminal.as_mut() else {
            return;
        };
        if self.editor.take_terminal_kill() {
            terminal.kill();
        }
        let chunks = terminal.drain();
        for chunk in chunks {
            self.editor.terminal_output(&chunk);
        }
        if let Some(bytes) = self.editor.take_terminal_input() {
            terminal.write(bytes);
        }
        if let Some((cols, rows)) = self.editor.take_terminal_resize() {
            terminal.resize(cols, rows);
        }
        if let Some(status) = terminal.exited() {
            self.terminal = None;
            self.editor.terminal_exited(status);
        }
    }

    fn tick_lsp(&mut self) {
        if self.lsp.is_none() {
            // A server is started for the file that is open, and the file a
            // session starts on is not the one it spends the day with: a
            // frontend sends `:e` along like any other command, so this is where
            // the *second* file gets its own server. The last file that was tried
            // is remembered, so a file with no server for it is not asked about
            // ten times a second.
            let wanted = self.editor.options.borrow().lsp
                && self.editor.buffer.path() != self.lsp_tried_for.as_deref();
            if !wanted {
                return;
            }
            let tried = self.editor.buffer.path().map(Path::to_path_buf);
            self.start_lsp();
            self.lsp_tried_for = tried;
            if self.lsp.is_none() {
                return;
            }
        }
        self.pump_lsp();
        self.sync_lsp();
        self.ask_lsp();
    }

    /// The frame that is due, if the editor has changed since the last one.
    pub fn frame_if_dirty(&mut self) -> Option<ServerMsg> {
        if !self.editor.redraw {
            return None;
        }
        Some(self.frame())
    }

    /// The editor, for inspection.
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// The editor, for mutation — a test driving a session needs this.
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// The language server state, for a test that has to stand in for what a
    /// server sent — the raw items a resolve is asked with, in particular.
    pub fn server_mut(&mut self) -> Option<&mut LspServer> {
        self.lsp.as_mut()
    }

    /// A full screen, for the very first frame.
    pub fn initial_frame(&mut self) -> ServerMsg {
        self.screen = None;
        self.frame()
    }

    /// Handle one client message.
    pub fn handle(&mut self, msg: ClientMsg) -> Response {
        match msg {
            ClientMsg::Shutdown => {
                return Response {
                    messages: Vec::new(),
                    quit: true,
                };
            }
            ClientMsg::Input(key) => self.editor.handle_key(key),
            ClientMsg::Pointer(pointer) => self.editor.handle_pointer(&pointer),
            // The clipboard belongs to the system, so the text goes back out to
            // whichever frontend can reach it.
            ClientMsg::Copy => self.editor.request_copy(),
            ClientMsg::Text(text) => {
                // A paste is the system clipboard arriving: `"+p` should be able
                // to put it back, and the kernel cannot ask for it itself.
                self.editor.note_clipboard_text(&text);
                self.editor.type_text(&text);
            }
            ClientMsg::Resize { cols, rows } => self.editor.handle_resize(cols, rows),
            ClientMsg::Command { name, params } => self.editor.handle_command(&name, &params),
        }

        let mut messages = Vec::new();
        // A copy the editor asked for, whether from a key or from the frontend's
        // own shortcut.
        if self.editor.clipboard.is_some() {
            messages.push(ServerMsg::Clipboard(
                self.editor.clipboard.take().unwrap_or_default(),
            ));
        }
        if self.editor.redraw {
            messages.push(self.frame());
        }
        let quit = self.editor.should_quit;
        if quit {
            messages.push(ServerMsg::Quit);
        }
        Response { messages, quit }
    }

    /// The next frame: the whole screen when there is nothing to compare
    /// against, and otherwise the rows that changed since the last one.
    fn frame(&mut self) -> ServerMsg {
        self.editor.redraw = false;
        let composed = self.editor.compose();
        let same_shape = self.screen.as_ref().is_some_and(|previous| {
            previous.width == composed.grid.width && previous.height == composed.grid.height
        });
        let message = match &self.screen {
            // A delta is only meaningful against a screen of the same shape:
            // a resize makes the previous one the wrong size to patch.
            Some(previous) if same_shape => ServerMsg::Rows {
                updates: composed.grid.rows_changed_since(previous),
                cursor: composed.cursor,
                cursor_visible: composed.cursor_visible,
                insert: composed.insert,
            },
            _ => ServerMsg::Redraw {
                grid: composed.grid.clone(),
                cursor: composed.cursor,
                cursor_visible: composed.cursor_visible,
                insert: composed.insert,
            },
        };
        self.screen = Some(composed.grid);
        message
    }
}

/// Run a session over a byte stream until the peer goes away or quits.
///
/// `input` is read on a helper thread; the main thread owns the editor and the
/// output, so the editor's state never crosses a thread boundary.
pub fn run_stdio(
    input: impl Read + Send + 'static,
    output: impl Write,
    init: InitConfig,
) -> Result<()> {
    let editor = Editor::new(init.resolve().as_deref())
        .map_err(|e| anyhow::anyhow!("failed to start the editor: {e}"))?;
    let mut session = Session::new(editor);
    let mut output = BufWriter::new(output);

    send(&mut output, &session.initial_frame())?;
    session.start_lsp();

    let (tx, rx) = unbounded::<ClientMsg>();
    thread::Builder::new()
        .name("inkstone-server-rpc-reader".to_string())
        .spawn(move || pump_input(input, tx))
        .context("failed to spawn the reader thread")?;

    // The language server speaks on its own schedule, so the loop waits for the
    // frontend only as long as it takes to notice: a message from the client
    // wakes it at once, and otherwise it looks every hundredth of a second —
    // which is far below what a person notices and far above what a server
    // costs.
    const TICK: std::time::Duration = std::time::Duration::from_millis(10);
    loop {
        let msg = match rx.recv_timeout(TICK) {
            Ok(msg) => Some(msg),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };
        let response = match msg {
            Some(msg) => session.handle(msg),
            None => Response {
                messages: Vec::new(),
                quit: false,
            },
        };
        for message in &response.messages {
            send(&mut output, message)?;
        }
        if response.quit {
            break;
        }
        // Always, and not only when a server is running: the file a session is
        // about arrives over the wire, so the tick that *starts* the server is
        // the one before there is one to run. Ticking with nothing to do costs a
        // comparison.
        session.tick();
        if let Some(new) = session.frame_if_dirty() {
            send(&mut output, &new)?;
        }
    }
    session.stop_lsp();
    session.stop_job();
    output.flush().context("failed to flush the output")?;
    Ok(())
}

fn send(output: &mut impl Write, msg: &ServerMsg) -> Result<()> {
    write_server_msg(output, msg).context("failed to write a frame")?;
    output.flush().context("failed to flush a frame")?;
    Ok(())
}

fn pump_input(mut input: impl Read, tx: Sender<ClientMsg>) {
    loop {
        match read_client_msg(&mut input) {
            Ok(Some(msg)) => {
                if tx.send(msg).is_err() {
                    return;
                }
            }
            Ok(None) => {
                // Clean end of stream: the frontend is gone.
                let _ = tx.send(ClientMsg::Shutdown);
                return;
            }
            Err(e) => {
                eprintln!("inkstone-server: rpc read error: {e}");
                let _ = tx.send(ClientMsg::Shutdown);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The lines of a document, the way a buffer counts them: the terminator of
    /// the last line is a terminator and not another line.
    fn lines(text: &str) -> Vec<String> {
        text.strip_suffix('\n')
            .unwrap_or(text)
            .split('\n')
            .map(|line| line.to_string())
            .collect()
    }

    #[test]
    fn a_payload_becomes_diagnostics_in_this_editor_s_terms() {
        let text = "let x = 1;\nlet y = 2;\n";
        let rows = lines(text);
        let payload = json!({
            "uri": "file:///a.rs",
            "diagnostics": [
                {
                    "range": {
                        "start": { "line": 0, "character": 4 },
                        "end": { "line": 0, "character": 5 }
                    },
                    "severity": 1,
                    "message": "unused variable"
                },
                {
                    "range": {
                        "start": { "line": 1, "character": 4 },
                        "end": { "line": 1, "character": 5 }
                    },
                    "message": "no severity said"
                }
            ]
        });
        let diagnostics = diagnostics_from(&payload, |row| rows[row].clone(), rows.len());
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].start, (0, 4));
        assert_eq!(diagnostics[0].end, (0, 5));
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert_eq!(diagnostics[0].message, "unused variable");
        // A server that says nothing about severity is still saying something is
        // wrong, and warning is what that reads as.
        assert_eq!(diagnostics[1].severity, Severity::Warning);

        // Nothing to say is nothing to say.
        assert!(diagnostics_from(&json!({}), |_| String::new(), 3).is_empty());
    }

    #[test]
    fn a_position_is_converted_from_utf16_and_clamped_into_the_file() {
        // A line with a character outside the basic plane: `é` is one code unit
        // and `😀` is two, so a server that counts them sees a longer line than
        // this editor does.
        let rows = lines("a😀b\nshort\n");
        let payload = json!({
            "diagnostics": [{
                "range": {
                    "start": { "line": 0, "character": 3 },
                    "end": { "line": 0, "character": 4 }
                },
                "severity": 2,
                "message": "after the emoji"
            }]
        });
        let diagnostics = diagnostics_from(&payload, |row| rows[row].clone(), rows.len());
        assert_eq!(diagnostics[0].start, (0, 2), "one character, two units");
        assert_eq!(diagnostics[0].end, (0, 3));

        // A line that is not in the file is clamped to the last one, the way a
        // server that is a keystroke behind is worth listening to anyway.
        let payload = json!({
            "diagnostics": [{
                "range": {
                    "start": { "line": 99, "character": 99 },
                    "end": { "line": 99, "character": 100 }
                },
                "severity": 1,
                "message": "gone"
            }]
        });
        let diagnostics = diagnostics_from(&payload, |row| rows[row].clone(), rows.len());
        assert_eq!(diagnostics[0].start.0, 1, "the last line");
        assert_eq!(diagnostics[0].start.1, 5, "and the end of it");
    }

    #[test]
    fn format_on_save_writes_what_the_server_laid_out() {
        // The same fixture, in a session of its own: what is being tested is a
        // *write*, and a write changes what is on disk for whatever runs next.
        let fixture = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
            .and_then(|dir| dir.parent().map(|dir| dir.to_path_buf()))
            .map(|dir| dir.join(format!("fake-lsp-server{}", std::env::consts::EXE_SUFFIX)));
        let Some(fixture) = fixture.filter(|path| path.is_file()) else {
            return;
        };
        std::env::set_var("INKSTONE_LSP", fixture.display().to_string());
        let dir = std::env::temp_dir().join("inkstone-server-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("saved.rs");
        std::fs::write(&path, "plain\n").unwrap();

        let mut editor = Editor::new(None).unwrap();
        editor.options.borrow_mut().lsp = true;
        editor.load_file(&path).unwrap();
        let mut session = Session::new(editor);
        assert!(session.start_lsp().is_some(), "the fixture was started");

        // Off by default: saving writes the buffer and nothing else.
        session.editor_mut().type_text("mine\n");
        session.editor_mut().execute_cmdline("w");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "mine\nplain\n",
            "the write happened, and no layout was asked for"
        );

        // On: the write happens, the layout is asked for, and *what came back is
        // what goes to disk* — so the file and the buffer agree, and the buffer
        // is clean.
        session.editor_mut().options.borrow_mut().formatonsave = true;
        session.editor_mut().type_text("again\n");
        session.editor_mut().execute_cmdline("w");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.editor().message.contains("asking") && std::time::Instant::now() < deadline {
            session.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            on_disk.starts_with("// formatted"),
            "the server's layout is what went to disk: {on_disk:?}"
        );
        assert!(
            on_disk.contains("again"),
            "and it is the text that was being saved: {on_disk:?}"
        );
        assert!(
            !session.editor().is_dirty(),
            "the buffer is clean, since what is on disk is what it holds"
        );
        assert!(
            session.editor().message.contains("written"),
            "{}",
            session.editor().message
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_session_hears_a_server_and_puts_it_where_it_belongs() {
        // The fake server the client crate builds: it answers `initialize` and
        // pushes one diagnostic about whatever file was opened.
        // The fixture is `inkstone-lsp`'s fake server, built beside this
        // crate's tests. When it is not there — a `cargo test -p
        // inkstone-server` on a tree where that crate was never built — there is
        // nothing to talk to, and the client's own tests cover the talking.
        let fixture = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
            .and_then(|dir| dir.parent().map(|dir| dir.to_path_buf()))
            .map(|dir| dir.join(format!("fake-lsp-server{}", std::env::consts::EXE_SUFFIX)));
        let Some(fixture) = fixture.filter(|path| path.is_file()) else {
            return;
        };
        std::env::set_var("INKSTONE_LSP", fixture.display().to_string());
        let dir = std::env::temp_dir().join("inkstone-server-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("heard.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();

        let mut editor = Editor::new(None).unwrap();
        editor.options.borrow_mut().lsp = true;
        editor.load_file(&path).unwrap();
        let mut session = Session::new(editor);
        assert!(session.start_lsp().is_some(), "the fixture was started");

        // Nothing is on the screen until the server has been heard, and then a
        // diagnostic is where it said.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.editor().diagnostics.is_empty() && std::time::Instant::now() < deadline {
            session.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let diagnostics = session.editor().diagnostics.clone();
        assert_eq!(
            diagnostics.len(),
            1,
            "the server pushed one, and a session that heard nothing heard it through {}",
            session.editor().message
        );
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert_eq!(diagnostics[0].message, "unused variable");
        assert_eq!(diagnostics[0].start, (0, 9));

        // Editing the file is a change the server hears about, as a whole
        // document and a new version.
        session.editor_mut().type_text(" ");
        assert!(session.editor().edit_count() > 0);
        session.sync_lsp();

        // A hover: the question is left by the kernel, asked by the session, and
        // the answer is what the message row says afterwards.
        session
            .editor_mut()
            .ask_server(Ask::Hover { row: 0, col: 3 });
        assert!(
            session.editor().message.contains("asking"),
            "{}",
            session.editor().message
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.editor().message.contains("asking") && std::time::Instant::now() < deadline {
            session.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            session.editor().message,
            "**fixture** A thing the server knows about.",
            "the hover, as one line"
        );

        // A definition: the answer names another file and a position in it, and
        // the session goes there — opening the file to do it. The fixture names
        // `nowhere/defined.rs`, relative to wherever the tests are run.
        let elsewhere = std::path::Path::new("nowhere/defined.rs");
        std::fs::create_dir_all(elsewhere.parent().expect("a folder")).unwrap();
        std::fs::write(elsewhere, "fn first() {}\nfn target_name() {}\n").unwrap();
        session
            .editor_mut()
            .ask_server(Ask::Definition { row: 0, col: 3 });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.editor().buffer.path().is_none()
            || !session
                .editor()
                .buffer
                .path()
                .is_some_and(|path| path.to_string_lossy().ends_with("defined.rs"))
        {
            session.tick();
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            session
                .editor()
                .buffer
                .path()
                .is_some_and(|path| path.to_string_lossy().ends_with("defined.rs")),
            "went to the file the server named: {:?}",
            session.editor().buffer.path()
        );
        assert_eq!(session.editor().cursor.row, 1, "and the line it named");
        assert_eq!(session.editor().cursor.col, 2, "and the column");
        let _ = std::fs::remove_dir_all("nowhere");

        // Completion, and then the resolve of the item the list is on: the
        // second question is about the item the first answer sent. The answer
        // the session has to hand is a completion one, so the raw items are
        // filled in the way the completion path fills them.
        // A line of its own, since a word is completed where the cursor is.
        session.editor_mut().type_text("\n");
        session.editor_mut().type_text("tar");
        let items = completion_items_from(&serde_json::json!({
            "items": [
                { "label": "target_name", "insertText": "target_name", "data": { "id": 7 } },
                { "label": "other", "insertText": "other" }
            ]
        }));
        session.editor_mut().set_completions(items);
        if let Some(server) = session.server_mut() {
            server.raw_completions = vec![
                serde_json::json!({ "label": "target_name", "insertText": "target_name",
                                    "data": { "id": 7 } }),
            ];
        }
        // The kernel leaves the item to ask about while it is ticked; the
        // session has the raw item to ask *with*.
        session.tick();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut detail = String::new();
        while std::time::Instant::now() < deadline {
            session.tick();
            detail = session
                .editor()
                .completion
                .as_ref()
                .map(|state| {
                    state
                        .all
                        .iter()
                        .chain(state.shown.iter())
                        .find(|item| item.label == "target_name")
                        .map(|item| item.detail.clone())
                        .unwrap_or_else(|| {
                            format!(
                                "no such item: {:?}",
                                state
                                    .all
                                    .iter()
                                    .map(|item| item.label.clone())
                                    .collect::<Vec<String>>()
                            )
                        })
                })
                .unwrap_or_else(|| "no completion list".to_string());
            if detail.contains("fixture") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            detail.contains("the fixture's word for target_name"),
            "the resolved detail reached the popup: {detail:?}"
        );

        // References: the answer is a list, and the message row is where a list
        // of places can go.
        session
            .editor_mut()
            .ask_server(Ask::References { row: 0, col: 3 });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.editor().message.contains("asking") && std::time::Instant::now() < deadline {
            session.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            session.editor().message,
            "2 references   defined.rs:1:4   other.rs:7:1",
            "both places, named by file"
        );
        // And the first of them is where the cursor is: a list of places is not
        // much use if the one that was asked about is a keystroke away.
        assert!(
            session
                .editor()
                .buffer
                .path()
                .is_some_and(|path| path.to_string_lossy().ends_with("defined.rs")),
            "the first reference is open: {:?}",
            session.editor().buffer.path()
        );
        assert_eq!(session.editor().cursor.row, 0, "on its line");
        // And the list is kept, so `:cnext` can walk it afterwards.
        assert_eq!(
            session.editor().places.len(),
            2,
            "both places are in the list: {:?}",
            session.editor().places
        );
        assert_eq!(
            session.editor().place_index,
            1,
            "and the walk is on the first"
        );

        // The whole project: a list of places in other files, filled and shown.
        session.editor_mut().ask_server(Ask::WorkspaceSymbols {
            row: 0,
            col: 4,
            query: None,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.editor().message.contains("asking") && std::time::Instant::now() < deadline {
            session.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            session.editor().places.len(),
            2,
            "both symbols are in the list: {:?}",
            session.editor().places
        );
        assert_eq!(
            session.editor().list,
            Some(inkstone_core::List::Places),
            "and it is shown"
        );
        assert!(
            session.editor().message.contains("2 symbols"),
            "{}",
            session.editor().message
        );
        // The window that showed them is a document with no file, so the next
        // question wants it out of the way — and closing it is `:cclose`.
        session.editor_mut().close_places_list();
        assert_eq!(session.editor().list, None);

        // Range formatting: the same answer about part of the file, and the
        // range travels in the request.
        session.editor_mut().ask_server(Ask::Format {
            row: 0,
            col: 0,
            from: Some(1),
            to: Some(1),
            save: false,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.editor().message.contains("asking") && std::time::Instant::now() < deadline {
            session.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            session.editor().buffer.text().contains("// ranged"),
            "the ranged edit was applied: {:?}",
            session.editor().buffer.text()
        );

        // Formatting: the answer is a list of edits, and they are applied.
        session.editor_mut().ask_server(Ask::Format {
            row: 0,
            col: 0,
            from: None,
            to: None,
            save: false,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.editor().message.contains("asking") && std::time::Instant::now() < deadline {
            session.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            session.editor().buffer.text().starts_with("// formatted"),
            "the edit was applied: {:?}",
            session.editor().buffer.text()
        );
        assert!(
            session.editor().message.contains("edit"),
            "{}",
            session.editor().message
        );

        // What the call takes: one line, in the message row.
        session
            .editor_mut()
            .ask_server(Ask::Signature { row: 0, col: 3 });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.editor().message.contains("asking") && std::time::Instant::now() < deadline {
            session.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            session.editor().message.contains("target_name"),
            "the signature reached the message row: {}",
            session.editor().message
        );

        // What the file holds: the answer names places, and the first is where
        // the cursor goes.
        session
            .editor_mut()
            .ask_server(Ask::Symbols { row: 0, col: 3 });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.editor().message.contains("asking") && std::time::Instant::now() < deadline {
            session.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            session.editor().message.contains("symbol"),
            "the symbols reached the message row: {}",
            session.editor().message
        );

        // A completion is asked of the server — the kernel leaves the question,
        // the session asks — and the answer becomes the kernel's items. A fresh
        // line, since a word is completed where the cursor is.
        session.editor_mut().type_text("\n");
        session.editor_mut().type_text("fi");
        session.editor_mut().complete(Some(1));
        assert!(
            session.editor().completion.is_some(),
            "the words in the file"
        );
        session.tick();
        assert!(session.lsp_is_asking(), "the server was asked");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !session
            .editor()
            .completion
            .as_ref()
            .is_some_and(|list| list.shown.iter().any(|item| item.label == "fixture_label"))
            && std::time::Instant::now() < deadline
        {
            session.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let labels: Vec<String> = session
            .editor()
            .completion
            .as_ref()
            .expect("a list")
            .shown
            .iter()
            .map(|item| item.label.clone())
            .collect();
        // The server's answer is what is offered now — a label the buffer never
        // had — and `fi` leaves only it, because the others do not start with
        // what has been typed.
        assert_eq!(labels, ["fixture_label", "fixture_snippet"]);
        assert_eq!(
            session.editor().completion.as_ref().expect("a list").shown[0].insert,
            "fixture_insert",
            "and what it would type is the server's"
        );

        session.stop_lsp();
    }

    #[test]
    fn a_completion_answer_becomes_items_in_this_editor_s_terms() {
        // The shape a real server uses: an object with a list in it, and items
        // that say what to type in one of three places.
        let answer = json!({
            "isIncomplete": false,
            "items": [
                { "label": "from_insert", "insertText": "inserted", "detail": "a function", "kind": 3 },
                { "label": "from_label", "kind": 6 },
                { "label": "from_edit", "textEdit": { "newText": "edited" } },
                { "label": "no_label_at_all", "insertText": "x" }
            ]
        });
        let items = completion_items_from(&answer);
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].label, "from_insert");
        assert_eq!(items[0].insert, "inserted", "what to type, when it says");
        assert_eq!(items[0].detail, "a function");
        assert_eq!(
            items[1].insert, "from_label",
            "and the label when it does not"
        );
        assert_eq!(items[2].insert, "edited", "a text edit is what to type too");

        // A bare list is the same answer in the other shape.
        let items = completion_items_from(&json!([{ "label": "plain" }]));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert, "plain");

        // And an answer with nothing in it is nothing, not a panic.
        assert!(completion_items_from(&json!({})).is_empty());
        assert!(completion_items_from(&Value::Null).is_empty());
        assert!(completion_items_from(&json!({ "items": "no" })).is_empty());
    }

    #[test]
    fn the_setting_in_an_init_file_starts_a_server() {
        // The chain the terminal goes through: `init.lua` sets `lsp`, the file
        // is opened, and the session starts a server for it — which is the one
        // thing a test with an editor built by hand does not cover.
        let fixture = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
            .and_then(|dir| dir.parent().map(|dir| dir.to_path_buf()))
            .map(|dir| dir.join(format!("fake-lsp-server{}", std::env::consts::EXE_SUFFIX)));
        let Some(fixture) = fixture.filter(|path| path.is_file()) else {
            return;
        };
        std::env::set_var("INKSTONE_LSP", fixture.display().to_string());
        let dir = std::env::temp_dir().join("inkstone-server-tests");
        std::fs::create_dir_all(&dir).unwrap();

        let init = dir.join("lsp-init.lua");
        std::fs::write(&init, "ed.set(\"lsp\", true)\n").unwrap();
        let path = dir.join("from-init.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();

        let mut editor = Editor::new(Some(&init)).unwrap();
        assert!(
            editor.options.borrow().lsp,
            "the init file's setting reached the editor"
        );
        editor.load_file(&path).unwrap();
        let mut session = Session::new(editor);
        assert!(
            session.start_lsp().is_some(),
            "and the session started a server for the file"
        );

        // The same question as everywhere else now goes through it.
        session
            .editor_mut()
            .ask_server(Ask::Hover { row: 0, col: 3 });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.editor().message.contains("asking") && std::time::Instant::now() < deadline {
            session.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            session.editor().message,
            "**fixture** A thing the server knows about."
        );
    }

    #[test]
    fn a_file_opened_after_the_start_gets_a_server_of_its_own() {
        // A frontend opens its file *after* the session is up — the daemon has
        // nothing when it starts — so a server decided only at startup is a
        // server that never runs.
        let fixture = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
            .and_then(|dir| dir.parent().map(|dir| dir.to_path_buf()))
            .map(|dir| dir.join(format!("fake-lsp-server{}", std::env::consts::EXE_SUFFIX)));
        let Some(fixture) = fixture.filter(|path| path.is_file()) else {
            return;
        };
        std::env::set_var("INKSTONE_LSP", fixture.display().to_string());
        let dir = std::env::temp_dir().join("inkstone-server-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("opened-later.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();

        let editor = Editor::new(None).unwrap();
        editor.options.borrow_mut().lsp = true;
        let mut session = Session::new(editor);

        // The frontend's file arrives the way a frontend sends it — a command
        // over the wire, not a call into the editor — and the tick after it
        // starts the server.
        let opened = session.handle(inkstone_rpc::ClientMsg::Command {
            name: "edit_file".to_string(),
            params: vec![inkstone_rpc::Param::Str(path.to_string_lossy().to_string())],
        });
        assert!(!opened.quit);
        session.tick();

        session
            .editor_mut()
            .ask_server(Ask::Hover { row: 0, col: 3 });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.editor().message.contains("asking") && std::time::Instant::now() < deadline {
            session.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            session.editor().message,
            "**fixture** A thing the server knows about.",
            "the server that was started late answers like any other"
        );
    }

    #[test]
    fn an_answer_of_edits_becomes_places_in_the_text() {
        let buffer = inkstone_core::Buffer::from_text("one\ntwo\nthree\n");
        // An edit on one line, and one whose columns are in UTF-16 code units —
        // which is what makes the conversion worth having.
        let answer = serde_json::json!([
            { "range": { "start": { "line": 1, "character": 0 },
                         "end": { "line": 1, "character": 3 } },
              "newText": "TWO" },
            { "range": { "start": { "line": 0, "character": 0 },
                         "end": { "line": 0, "character": 0 } },
              "newText": "// " }
        ]);
        let edits = edits_from(&answer, &buffer);
        assert_eq!(edits.len(), 2);
        assert_eq!(
            edits[0],
            (4, 7, "TWO".to_string()),
            "line two, columns 0..3"
        );
        assert_eq!(edits[1], (0, 0, "// ".to_string()), "the top of the file");
        // Nothing, and a shape that is not a list of edits.
        assert!(edits_from(&serde_json::json!([]), &buffer).is_empty());
        assert!(edits_from(&Value::Null, &buffer).is_empty());
    }

    #[test]
    fn a_query_can_be_named_or_taken_from_the_cursor() {
        // The command's own query wins, and the cursor's word is the fallback —
        // which is the whole of the rule, and it lives in `ask_lsp`.
        let named = Ask::WorkspaceSymbols {
            row: 0,
            col: 4,
            query: Some("counter".to_string()),
        };
        assert_eq!(named.position(), (0, 4));
        let unnamed = Ask::WorkspaceSymbols {
            row: 1,
            col: 2,
            query: None,
        };
        assert_eq!(unnamed.position(), (1, 2));
        assert_ne!(named, unnamed, "the query is part of what was asked");
    }

    #[test]
    fn the_word_under_the_cursor_is_the_query() {
        assert_eq!(word_at("let target_name = 1;", 6), "target_name");
        assert_eq!(
            word_at("let target_name = 1;", 4),
            "target_name",
            "at the start"
        );
        assert_eq!(
            word_at("let target_name = 1;", 15),
            "target_name",
            "at the end"
        );
        assert_eq!(word_at("let x = 1;", 4), "x");
        assert_eq!(
            word_at("nothing here", 7),
            "nothing",
            "the word, not the space"
        );
        assert_eq!(word_at("  ", 1), "", "no word, no query");
        assert_eq!(word_at("", 0), "");
        // Past the end of the line is not a panic: the word is the last one.
        assert_eq!(word_at("word", 99), "word");
        assert_eq!(word_at("word ", 99), "", "past the space there is nothing");
    }

    #[test]
    fn a_signature_answer_names_the_call_and_the_part_being_typed() {
        // A parameter named by its text.
        let answer = serde_json::json!({
            "signatures": [{
                "label": "fn f(a: i32, b: i32)",
                "parameters": [{ "label": "a: i32" }, { "label": "b: i32" }]
            }],
            "activeSignature": 0,
            "activeParameter": 1
        });
        assert_eq!(
            signature_text(&answer),
            "fn f(a: i32, b: i32)   (b: i32 now)"
        );
        // A parameter named by offsets into the label, which is the other way a
        // server may write one.
        let answer = serde_json::json!({
            "signatures": [{
                "label": "fn f(a: i32)",
                "parameters": [{ "label": [5, 11] }]
            }],
            "activeSignature": 0,
            "activeParameter": 0
        });
        assert_eq!(signature_text(&answer), "fn f(a: i32)   (a: i32 now)");
        // Offsets that do not fit the label fall back to the label itself.
        let answer = serde_json::json!({
            "signatures": [{ "label": "short", "parameters": [{ "label": [0, 99] }] }],
            "activeParameter": 0
        });
        assert_eq!(signature_text(&answer), "short");
        // No active parameter, several signatures, and none at all.
        let answer = serde_json::json!({
            "signatures": [{ "label": "one()" }, { "label": "two(x: i32)" }],
            "activeSignature": 1
        });
        assert_eq!(signature_text(&answer), "two(x: i32)");
        assert_eq!(signature_text(&serde_json::json!({})), "no signature");
        assert_eq!(
            signature_text(&serde_json::json!({ "signatures": [] })),
            "no signature"
        );
    }

    #[test]
    fn a_symbol_answer_reads_as_a_list_of_names() {
        // The nested shape: a class and the methods inside it.
        let nested = serde_json::json!([
            {
                "name": "Counter",
                "kind": 5,
                "range": { "start": { "line": 2, "character": 0 } },
                "children": [
                    { "name": "new", "kind": 9, "range": { "start": { "line": 3, "character": 4 } } },
                    { "name": "next", "kind": 6, "range": { "start": { "line": 7, "character": 4 } } }
                ]
            }
        ]);
        assert_eq!(
            symbols_from(&nested),
            vec![
                ("Counter".to_string(), "class".to_string(), 2),
                ("new".to_string(), "constructor".to_string(), 3),
                ("next".to_string(), "method".to_string(), 7),
            ]
        );
        // The flat shape: a `location` holds the position instead.
        let flat = serde_json::json!([
            { "name": "main", "kind": 12,
              "location": { "uri": "file:///a/b.rs", "range": { "start": { "line": 0, "character": 3 } } } }
        ]);
        assert_eq!(
            symbols_from(&flat),
            vec![("main".to_string(), "function".to_string(), 0)]
        );
        // A kind that is not one of the protocol's is left as its number.
        let odd = serde_json::json!([{ "name": "x", "kind": 99, "range": { "start": { "line": 1, "character": 0 } } }]);
        assert_eq!(
            symbols_from(&odd),
            vec![("x".to_string(), "99".to_string(), 1)]
        );
        assert_eq!(symbols_from(&Value::Null), Vec::new());
        assert_eq!(symbols_text(&serde_json::json!([])), "no symbols");
        assert_eq!(symbols_text(&flat), "1 symbol   main (function) on line 1");
        assert!(
            symbols_text(&nested).starts_with("3 symbols   Counter (class) on line 3"),
            "{}",
            symbols_text(&nested)
        );
    }

    #[test]
    fn a_hover_is_one_line_of_whatever_shape_it_came_in() {
        // All four shapes the protocol allows, and one that says nothing.
        assert_eq!(
            hover_text_from(&json!({ "contents": "plain words" })),
            "plain words"
        );
        assert_eq!(
            hover_text_from(&json!({ "contents": { "value": "marked words" } })),
            "marked words"
        );
        assert_eq!(
            hover_text_from(&json!({ "contents": ["one", { "value": "two" }] })),
            "one  two"
        );
        assert_eq!(
            hover_text_from(&json!({
                "contents": { "kind": "markdown", "value": "**bold**\n\nand more" }
            })),
            "**bold** and more",
            "a status line is one line"
        );
        assert_eq!(hover_text_from(&json!({})), "nothing to say about this");
        assert_eq!(hover_text_from(&Value::Null), "nothing to say about this");
    }

    #[test]
    fn a_reference_answer_reads_as_a_list_of_places() {
        // The shapes a server may answer with: a list, a single location, and
        // links under other names.
        assert_eq!(
            locations_from(&serde_json::json!([
                { "uri": "file:///a/b.rs", "range": { "start": { "line": 1, "character": 2 } } },
                { "uri": "file:///a/c.rs", "range": { "start": { "line": 3, "character": 0 } } }
            ])),
            vec![
                ("file:///a/b.rs".to_string(), 1, 2),
                ("file:///a/c.rs".to_string(), 3, 0),
            ]
        );
        assert_eq!(
            locations_from(&serde_json::json!({
                "uri": "file:///a/b.rs",
                "range": { "start": { "line": 0, "character": 0 } }
            })),
            vec![("file:///a/b.rs".to_string(), 0, 0)],
            "one place is still a list of one"
        );
        assert_eq!(
            locations_from(&serde_json::json!({
                "targetUri": "file:///a/d.rs",
                "targetRange": { "start": { "line": 2, "character": 5 } }
            })),
            vec![("file:///a/d.rs".to_string(), 2, 5)]
        );
        assert_eq!(locations_from(&Value::Null), Vec::new(), "nothing to say");
        // The answer as one line, named by file.
        assert_eq!(references_text(&serde_json::json!([])), "no references");
        assert_eq!(
            references_text(&serde_json::json!([
                { "uri": "file:///a/b.rs", "range": { "start": { "line": 1, "character": 2 } } }
            ])),
            "1 reference   b.rs:2:3"
        );
        assert_eq!(
            references_text(&serde_json::json!([
                { "uri": "file:///a/b.rs", "range": { "start": { "line": 0, "character": 0 } } },
                { "uri": "file:///a/c.rs", "range": { "start": { "line": 3, "character": 4 } } }
            ])),
            "2 references   b.rs:1:1   c.rs:4:5"
        );
    }

    #[test]
    fn a_definition_points_at_a_file_and_a_position() {
        // One location, a list, and a link: the same answer in three shapes.
        let one =
            json!({ "uri": "file:///a/b.rs", "range": { "start": { "line": 4, "character": 6 } } });
        assert_eq!(
            definition_from(&one),
            Some(("file:///a/b.rs".to_string(), 4, 6))
        );
        assert_eq!(
            definition_from(&json!([one])),
            Some(("file:///a/b.rs".to_string(), 4, 6))
        );
        assert_eq!(
            definition_from(&json!({
                "targetUri": "file:///c/d.rs",
                "targetRange": { "start": { "line": 0, "character": 1 } }
            })),
            Some(("file:///c/d.rs".to_string(), 0, 1))
        );
        assert_eq!(definition_from(&json!({})), None, "nothing to go to");

        // The path a URI names, on this platform.
        let path = path_from_uri("file:///C:/a/b.rs").expect("a path");
        assert!(path.to_string_lossy().contains("C:"), "{path:?}");
        assert!(path.to_string_lossy().ends_with("b.rs"), "{path:?}");
        assert_eq!(path_from_uri("http://example.com/x"), None, "not a file");
    }

    #[test]
    fn a_server_is_not_started_when_nobody_asked_for_one() {
        let editor = Editor::new(None).unwrap();
        assert!(!editor.options.borrow().lsp, "off by default");
        let mut session = Session::new(editor);
        assert_eq!(session.start_lsp(), None);
        assert!(!session.lsp_is_running());
        // And a tick with no server is not a panic and not a frame of its own:
        // the first frame is the first frame, and after it nothing moves.
        session.initial_frame();
        session.tick();
        assert!(session.frame_if_dirty().is_none());
    }
}
