//! A fake language server, for the client's tests.
//!
//! It speaks the framing, answers `initialize` and the `textDocument/`
//! notifications with notifications of its own, and pushes one diagnostic
//! nobody asked for. It is not part of the editor: it exists so that the
//! client can be tested against a real process and real pipes.

use std::io::{BufReader, Write};

use inkstone_lsp::framing;
use serde_json::{json, Value};

/// A minimal server: read messages, answer a few, and push one nobody asked for.
///
/// It is a binary of its own rather than half of the test, because a test
/// harness writes to standard output — and the protocol *is* standard output.
/// The test finds it through `CARGO_BIN_EXE_…`, which is what that variable is
/// for.
fn main() {
    let mut out = std::io::stdout();
    let mut input = BufReader::new(std::io::stdin());

    while let Ok(Some(value)) = framing::read_message(&mut input) {
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        match method.as_str() {
            "initialize" => {
                // A response *and* a notification, which is what a real server
                // does: the response carries the id, the notification does not.
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "result": { "capabilities": {} }
                    }),
                );
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "fake/initialized",
                        "params": {
                            "name": params["clientInfo"]["name"],
                            "root": params["rootUri"],
                            "capabilities": params["capabilities"]
                        }
                    }),
                );
            }
            "initialized" => {}
            "textDocument/didOpen" => {
                let document = &params["textDocument"];
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "fake/opened",
                        "params": {
                            "uri": document["uri"],
                            "language": document["languageId"],
                            "text": document["text"]
                        }
                    }),
                );
                // A server pushes diagnostics whenever it likes, and this one
                // likes right after a file appears.
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/publishDiagnostics",
                        "params": {
                            "uri": document["uri"],
                            "diagnostics": [{
                                "range": {
                                    "start": { "line": 0, "character": 9 },
                                    "end": { "line": 0, "character": 10 }
                                },
                                "severity": 1,
                                "message": "unused variable"
                            }]
                        }
                    }),
                );
            }
            "textDocument/didChange" => {
                let document = &params["textDocument"];
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "fake/changed",
                        "params": {
                            "uri": document["uri"],
                            "version": document["version"],
                            "text": params["contentChanges"][0]["text"]
                        }
                    }),
                );
                // A second problem, so that a client which sent the change can
                // tell the difference between "heard" and "not heard".
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/publishDiagnostics",
                        "params": {
                            "uri": document["uri"],
                            "diagnostics": [{
                                "range": {
                                    "start": { "line": 0, "character": 9 },
                                    "end": { "line": 0, "character": 10 }
                                },
                                "severity": 1,
                                "message": "unused variable"
                            }, {
                                "range": {
                                    "start": { "line": 0, "character": 0 },
                                    "end": { "line": 0, "character": 3 }
                                },
                                "severity": 2,
                                "message": "changed, so here is another"
                            }]
                        }
                    }),
                );
            }
            "textDocument/didSave" => {}
            "textDocument/didClose" => {
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "fake/closed",
                        "params": { "uri": params["textDocument"]["uri"] }
                    }),
                );
            }
            "textDocument/completion" => {
                // A list with the two shapes a real server uses: `insertText`
                // that differs from the label, and a `TextEdit` whose new text
                // is what should be typed.
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "result": {
                            "isIncomplete": false,
                            // No `detail`: a real server leaves that to
                            // `completionItem/resolve`, which this one answers.
                            "items": [{
                                "label": "fixture_label",
                                "insertText": "fixture_insert",
                                "kind": 3
                            }, {
                                // A snippet: the text has a placeholder in it,
                                // which is what `insertTextFormat` 2 means.
                                "label": "fixture_snippet",
                                "insertText": "fixture_${1:label}()",
                                "insertTextFormat": 2,
                                "kind": 3
                            }, {
                                "label": "count",
                                "kind": 6
                            }, {
                                "label": "from_a_text_edit",
                                "textEdit": {
                                    "range": {
                                        "start": { "line": 0, "character": 0 },
                                        "end": { "line": 0, "character": 0 }
                                    },
                                    "newText": "edited_in"
                                }
                            }]
                        }
                    }),
                );
            }
            "textDocument/hover" => {
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "result": {
                            "contents": {
                                "kind": "markdown",
                                "value": "**fixture**\n\nA thing the server knows about."
                            }
                        }
                    }),
                );
            }
            "textDocument/definition" => {
                // An absolute path under wherever this server was started, so a
                // client can be checked against a file it can actually open.
                let elsewhere = std::env::current_dir()
                    .unwrap_or_default()
                    .join("nowhere")
                    .join("defined.rs");
                let uri = format!("file:///{}", elsewhere.to_string_lossy().replace('\\', "/"));
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "result": [{
                            "uri": uri,
                            "range": {
                                "start": { "line": 1, "character": 2 },
                                "end": { "line": 1, "character": 5 }
                            }
                        }]
                    }),
                );
            }
            "textDocument/references" => {
                let elsewhere = std::env::current_dir()
                    .unwrap_or_default()
                    .join("nowhere")
                    .join("defined.rs");
                let uri = format!("file:///{}", elsewhere.to_string_lossy().replace('\\', "/"));
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "result": [
                            {
                                "uri": uri,
                                "range": {
                                    "start": { "line": 0, "character": 3 },
                                    "end": { "line": 0, "character": 14 }
                                }
                            },
                            {
                                "uri": "file:///nowhere/other.rs",
                                "range": {
                                    "start": { "line": 6, "character": 0 },
                                    "end": { "line": 6, "character": 11 }
                                }
                            }
                        ]
                    }),
                );
            }
            "completionItem/resolve" => {
                // The item arrives as it was sent, and comes back with a detail
                // filled in — which is the whole point of asking.
                let mut item = value.get("params").cloned().unwrap_or(json!({}));
                if let Some(object) = item.as_object_mut() {
                    object.insert(
                        "detail".to_string(),
                        Value::String(format!(
                            "the fixture's word for {}",
                            object
                                .get("label")
                                .and_then(Value::as_str)
                                .unwrap_or("something")
                        )),
                    );
                }
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "result": item
                    }),
                );
            }
            "textDocument/documentSymbol" => {
                // One nested and one flat, since a server may send either.
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "result": [
                            {
                                "name": "fixture_thing",
                                "kind": 5,
                                "range": { "start": { "line": 0, "character": 0 } },
                                "children": [
                                    { "name": "fixture_method", "kind": 6,
                                      "range": { "start": { "line": 1, "character": 4 } } }
                                ]
                            }
                        ]
                    }),
                );
            }
            "textDocument/signatureHelp" => {
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "result": {
                            "signatures": [{
                                "label": "fn target_name(count: usize) -> String",
                                "parameters": [
                                    { "label": "count: usize" }
                                ]
                            }],
                            "activeSignature": 0,
                            "activeParameter": 0
                        }
                    }),
                );
            }
            "workspace/symbol" => {
                let elsewhere = std::env::current_dir()
                    .unwrap_or_default()
                    .join("nowhere")
                    .join("defined.rs");
                let uri = format!("file:///{}", elsewhere.to_string_lossy().replace('\\', "/"));
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "result": [
                            {
                                "name": "target_name",
                                "kind": 12,
                                "location": { "uri": uri,
                                              "range": { "start": { "line": 0, "character": 3 } } }
                            },
                            {
                                "name": "other_thing",
                                "kind": 5,
                                "location": { "uri": "file:///nowhere/other.rs",
                                              "range": { "start": { "line": 4, "character": 0 } } }
                            }
                        ]
                    }),
                );
            }
            "textDocument/formatting" => {
                // One edit, at the top of the file, which is enough to see that
                // the answer is applied and that it is one step to undo.
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "result": [{
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 0 }
                            },
                            "newText": "// formatted\n"
                        }]
                    }),
                );
            }
            "textDocument/rangeFormatting" => {
                // An edit at the start of the range the client asked about, so a
                // test can see that the range travelled.
                let range = &value["params"]["range"];
                let line = range["start"]["line"].as_u64().unwrap_or(0);
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "result": [{
                            "range": {
                                "start": { "line": line, "character": 0 },
                                "end": { "line": line, "character": 0 }
                            },
                            "newText": "// ranged\n"
                        }]
                    }),
                );
            }
            "shutdown" => {
                say(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "result": Value::Null
                    }),
                );
            }
            "exit" => break,
            _ => {}
        }
    }
}

/// Write one message, and make it leave.
///
/// What goes wrong goes to standard error: standard output is the protocol, and
/// a fixture that fails silently is a fixture that costs an hour.
fn say(out: &mut impl Write, value: Value) {
    if let Err(error) = framing::write_message(out, &value) {
        eprintln!("fake-lsp-server: {error}");
    }
    if let Err(error) = out.flush() {
        eprintln!("fake-lsp-server: flush: {error}");
    }
}
