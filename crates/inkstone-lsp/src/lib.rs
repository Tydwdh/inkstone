//! A language server, spoken to over its pipes.
//!
//! The protocol is JSON-RPC 2.0 carried by a framing nobody would invent twice:
//! a `Content-Length` header in bytes, a blank line, and then exactly that many
//! bytes of JSON. That framing is the whole of [`framing`], and it is tested
//! against an in-memory stream because a buffer is easier to be sure about than
//! a subprocess.
//!
//! # What this is, and what it is not
//!
//! This is the client half and nothing else: it starts a server, says what the
//! editor is, tells the server what is open and what changed, and hands whatever
//! comes back to a channel. It knows nothing about editors, files, or the
//! `inkstone` kernel — the daemon owns all three, and translates the protocol's
//! positions into the kernel's before anything else sees them. Design note:
//! `docs/lsp.md`.
//!
//! # Positions are not characters
//!
//! A position in this protocol is a line and an offset in **UTF-16 code units**.
//! Everywhere else in this repository a column is a character. The two agree
//! until an emoji, which is one character and two code units, so the conversion
//! lives here in [`position`], with a test that uses a character outside the
//! basic plane.

pub mod framing;
pub mod position;

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use crossbeam_channel::{unbounded, Receiver};
use serde_json::{json, Value};

pub use framing::FramingError;

/// What a language server said, as it said it.
///
/// The messages this client acts on are notifications — the server pushes
/// diagnostics whenever it likes — so what comes back is a stream of JSON
/// values, with no correlation to anything sent. A request that answers
/// something (completion, a definition) will need the `id` matched up; that is
/// not here yet.
#[derive(Debug, Clone)]
pub struct Message(pub Value);

impl Message {
    /// The method of a notification, or `None` for a response.
    pub fn method(&self) -> Option<&str> {
        self.0.get("method")?.as_str()
    }

    /// The id of a response, or `None` for a notification.
    pub fn id(&self) -> Option<i64> {
        self.0.get("id")?.as_i64()
    }

    /// The parameters, if it has any.
    pub fn params(&self) -> Option<&Value> {
        self.0.get("params")
    }
}

/// A running language server, and the channel it talks into.
pub struct Client {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Message>,
    next_id: i64,
    /// What has been sent, so the same file is not opened twice.
    opened: Vec<String>,
    /// The answers to requests that have been made, by the id that was sent.
    ///
    /// A server answers when it is ready to, which may be before the client has
    /// looked for the answer — a completion that arrives in the same millisecond
    /// as the keystroke would otherwise be lost — so the answers wait here.
    answers: HashMap<i64, Value>,
}

impl Client {
    /// Start `command` and take hold of its pipes.
    ///
    /// The command is a shell-free word list: the first word is the program and
    /// the rest are its arguments, which is what a setting can honestly say. Its
    /// standard error is left alone, so a server that complains is heard.
    pub fn spawn(command: &str) -> std::io::Result<Self> {
        let mut words = command.split_whitespace();
        let Some(program) = words.next() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty language server command",
            ));
        };
        let mut child = Command::new(program)
            .args(words)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "no stdin for the server")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "no stdout for the server")
        })?;

        // A thread per server: the protocol is push, and a reader that only
        // reads when the editor happens to ask would lose everything the server
        // said in between.
        let (sender, messages) = unbounded();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match framing::read_message(&mut reader) {
                    Ok(Some(value)) => {
                        if sender.send(Message(value)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        // The stream is not the protocol any more. The reader
                        // stops, and the reason is said out loud rather than
                        // leaving a client that is quietly deaf.
                        eprintln!("inkstone-lsp: {error}");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            messages,
            next_id: 1,
            opened: Vec::new(),
            answers: HashMap::new(),
        })
    }

    /// Everything the server has said since the last look, without waiting.
    pub fn try_recv(&self) -> Vec<Message> {
        let mut out = Vec::new();
        while let Ok(message) = self.messages.try_recv() {
            out.push(message);
        }
        out
    }

    /// Take in everything the server has said: answers are kept by id, and
    /// notifications — which is how diagnostics arrive — come back.
    pub fn poll(&mut self) -> Vec<Message> {
        let mut notifications = Vec::new();
        for message in self.try_recv() {
            match message.id() {
                // A response: the id it answers, and its result or its error.
                Some(id) => {
                    let answer = message
                        .0
                        .get("result")
                        .cloned()
                        .or_else(|| message.0.get("error").cloned())
                        .unwrap_or(Value::Null);
                    self.answers.insert(id, answer);
                }
                None => notifications.push(message),
            }
        }
        notifications
    }

    /// The answer to a request, if it has arrived. Asking for it takes it, so a
    /// second look does not see the answer to a question nobody is asking any
    /// more.
    pub fn answer(&mut self, id: i64) -> Option<Value> {
        self.answers.remove(&id)
    }

    /// Make a request: an id, a method, and its parameters. The answer is picked
    /// up later with [`Client::answer`].
    pub fn request(&mut self, method: &str, params: Value) -> Result<i64, FramingError> {
        let id = self.take_id();
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;
        Ok(id)
    }

    /// Say who we are and wait for the server to agree.
    pub fn initialize(&mut self, root: Option<&str>) -> Result<(), FramingError> {
        let root_uri = match root {
            Some(path) => json!(uri_from_path(path)),
            None => Value::Null,
        };
        let id = self.take_id();
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "clientInfo": { "name": "inkstone" },
                "rootUri": root_uri,
                "capabilities": {
                    // Text is what this client sends: whole documents, never
                    // offsets into them.
                    "textDocument": {
                        "synchronization": { "dynamicRegistration": false },
                        "publishDiagnostics": { "relatedInformation": false }
                    }
                }
            }
        }))?;
        self.notify("initialized", json!({}))
    }

    /// Tell the server a file is open, with everything that is in it.
    pub fn did_open(
        &mut self,
        uri: &str,
        language_id: &str,
        text: &str,
        version: i64,
    ) -> Result<(), FramingError> {
        if !self.opened.iter().any(|open| open == uri) {
            self.opened.push(uri.to_string());
        }
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": version,
                    "text": text
                }
            }),
        )
    }

    /// Tell the server everything that is in a file now.
    pub fn did_change(&mut self, uri: &str, text: &str, version: i64) -> Result<(), FramingError> {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                // Whole documents: the client does not compute diffs, and a
                // server that is handed one is being told something it can
                // check.
                "contentChanges": [ { "text": text } ]
            }),
        )
    }

    /// Tell the server a file has been written.
    pub fn did_save(&mut self, uri: &str, text: &str) -> Result<(), FramingError> {
        self.notify(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": uri }, "text": text }),
        )
    }

    /// Tell the server a file is no longer open.
    pub fn did_close(&mut self, uri: &str) -> Result<(), FramingError> {
        self.opened.retain(|open| open != uri);
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        )
    }

    /// Ask the server to stop, and wait for it to go.
    pub fn shutdown(&mut self) -> Result<(), FramingError> {
        let id = self.take_id();
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "shutdown",
            "params": Value::Null
        }))?;
        self.notify("exit", Value::Null)?;
        let _ = self.child.wait();
        Ok(())
    }

    /// The files this client has told the server are open.
    pub fn opened(&self) -> &[String] {
        &self.opened
    }

    fn take_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), FramingError> {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn send(&mut self, value: Value) -> Result<(), FramingError> {
        framing::write_message(&mut self.stdin, &value)?;
        self.stdin.flush()?;
        Ok(())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // A server that is not asked to stop is killed, so a session that ends
        // badly does not leave a process behind.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The `file://` URI of a path, as the protocol spells one.
///
/// Windows paths are made into the form the protocol expects rather than the
/// one the platform uses: `C:\a\b` is `file:///C:/a/b`.
pub fn uri_from_path(path: &str) -> String {
    let mut text = path.replace('\\', "/");
    if !text.starts_with('/') {
        text.insert(0, '/');
    }
    format!("file://{text}")
}
