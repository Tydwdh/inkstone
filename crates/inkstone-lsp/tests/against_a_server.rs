//! The client, against a real process over real pipes.
//!
//! Pipes are where the framing bugs live: a message written with the wrong
//! length, a flush that never happened, headers read as line endings the platform
//! does not use. So the test starts a real server — the `fake-lsp-server` binary
//! this crate builds beside its tests — and speaks the protocol to it.

use inkstone_lsp::{uri_from_path, Client, Message};
use serde_json::Value;

#[test]
fn the_client_talks_to_a_server_over_its_pipes() {
    // Cargo builds this beside the test and tells the test where it is.
    let server = env!("CARGO_BIN_EXE_fake-lsp-server");
    let mut client = Client::spawn(server).expect("the fake server runs");

    // `initialize`: the server is told who is talking and what it can do.
    let mut said = Said::default();
    client.initialize(Some("D:/projects/thing")).expect("sends");
    said.wait_for(&client, "fake/initialized");
    let init = said.params("fake/initialized");
    assert_eq!(init["name"], "inkstone", "{init}");
    assert_eq!(init["root"], "file:///D:/projects/thing", "{init}");
    assert!(init["capabilities"]["textDocument"]["publishDiagnostics"].is_object());

    // A file is opened: the server is told everything that is in it, and it
    // pushes diagnostics back without being asked.
    let uri = uri_from_path("D:/projects/thing/src/main.rs");
    client
        .did_open(&uri, "rust", "fn main() {}\n", 1)
        .expect("sends");
    said.wait_for(&client, "fake/opened");
    let opened = said.params("fake/opened");
    assert_eq!(opened["uri"], uri.as_str());
    assert_eq!(opened["language"], "rust");
    assert_eq!(opened["text"], "fn main() {}\n");

    said.wait_for(&client, "textDocument/publishDiagnostics");
    let diagnostics = said.params("textDocument/publishDiagnostics");
    assert_eq!(diagnostics["uri"], uri.as_str());
    assert_eq!(diagnostics["diagnostics"][0]["message"], "unused variable");
    assert_eq!(diagnostics["diagnostics"][0]["range"]["start"]["line"], 0);
    assert_eq!(
        diagnostics["diagnostics"][0]["range"]["start"]["character"],
        9
    );

    // A change sends the whole document again, and its version.
    client
        .did_change(&uri, "fn main() { let x = 1; }\n", 2)
        .expect("sends");
    said.wait_for(&client, "fake/changed");
    let changed = said.params("fake/changed");
    assert_eq!(changed["text"], "fn main() { let x = 1; }\n");
    assert_eq!(changed["version"], 2);

    // A request that answers something: the id is matched to the answer, and
    // the answer is an answer even if it arrives before anyone looks.
    let id = client
        .request(
            "textDocument/completion",
            serde_json::json!({ "textDocument": { "uri": uri } }),
        )
        .expect("sends");
    let answer = wait_for_answer(&mut client, id);
    assert_eq!(answer["items"][0]["label"], "fixture_label");
    assert_eq!(answer["items"][0]["insertText"], "fixture_insert");
    assert!(client.answer(id).is_none(), "and asking for it took it");

    // What the client knows it has open, and what it says when a file closes.
    assert_eq!(client.opened(), std::slice::from_ref(&uri));
    client.did_close(&uri).expect("sends");
    assert!(client.opened().is_empty());
    said.wait_for(&client, "fake/closed");
    assert_eq!(said.params("fake/closed")["uri"], uri.as_str());

    // A server that is never asked to stop does not outlive the client.
    drop(client);
}

/// Wait for the answer to a request, giving the process time to write it.
fn wait_for_answer(client: &mut Client, id: i64) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        client.poll();
        if let Some(answer) = client.answer(id) {
            return answer;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no answer to request {id}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Everything the server has said, kept as it arrives.
///
/// A notification nobody is waiting for still has to be there when it *is*
/// looked at, so nothing is thrown away between one wait and the next: the
/// server decides when to speak, and the test only decides when to listen.
#[derive(Default)]
struct Said(Vec<Message>);

impl Said {
    /// Wait for a message with a method, and give back that one.
    fn wait_for(&mut self, client: &Client, method: &str) -> Message {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            for message in client.try_recv() {
                self.0.push(message);
            }
            if let Some(found) = self.0.iter().find(|m| m.method() == Some(method)) {
                return found.clone();
            }
            if std::time::Instant::now() >= deadline {
                let seen: Vec<&str> = self.0.iter().filter_map(|m| m.method()).collect();
                panic!("waited for {method}, and heard {seen:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// The parameters of the first message with a method.
    fn params(&self, method: &str) -> Value {
        self.0
            .iter()
            .find(|m| m.method() == Some(method))
            .and_then(|m| m.params().cloned())
            .unwrap_or(Value::Null)
    }
}
