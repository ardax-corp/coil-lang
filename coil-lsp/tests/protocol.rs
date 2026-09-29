//! Protocol-level tests: spawn `coil-lsp` and speak JSON-RPC over stdio.
//!
//! Unix-only: the tests build `file://` URIs from raw paths.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
    /// Notifications seen while waiting for responses.
    notifications: Vec<Value>,
}

impl Client {
    fn spawn(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_coil-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn coil-lsp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
            notifications: Vec::new(),
        };
        let root_uri = format!("file://{}", root.display());
        client.request("initialize", json!({ "rootUri": root_uri, "capabilities": {} }));
        client.notify("initialized", json!({}));
        client
    }

    fn send(&mut self, message: Value) {
        let body = message.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut length = 0;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).unwrap();
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = value.parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        self.stdout.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let message = self.read();
            if message.get("id") == Some(&json!(id)) {
                return message;
            }
            self.notifications.push(message);
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Messages of every `publishDiagnostics` seen so far for `uri`.
    fn diagnostics_for(&self, uri: &str) -> Vec<Vec<String>> {
        self.notifications
            .iter()
            .filter(|n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == uri)
            .map(|n| {
                n["params"]["diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|d| d["message"].as_str().unwrap().to_owned())
                    .collect()
            })
            .collect()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coil-lsp-protocol-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    // macOS temp dirs are symlinks; the server reports canonical paths.
    dir.canonicalize().unwrap()
}

fn uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

const CLASS_SOURCE: &str = "class P {\n    pub x: int,\n}\n\nimpl P {\n    pub fn get() -> int {\n        return self.x;\n    }\n}\n\nfn main() {\n    let p = new P(1);\n    let _ = p.get();\n}\n";

#[test]
fn unknown_request_gets_method_not_found() {
    let dir = project("unknown", &[("src/main.hy", CLASS_SOURCE)]);
    let mut client = Client::spawn(&dir);
    let reply = client.request(
        "textDocument/nonexistent",
        json!({ "textDocument": { "uri": uri(&dir.join("src/main.hy")) } }),
    );
    assert_eq!(reply["error"]["code"], json!(-32601), "reply={reply}");
    // The server is still alive and answering.
    let reply = client.request("shutdown", Value::Null);
    assert!(reply.get("error").is_none(), "reply={reply}");
}

#[test]
fn edits_do_not_produce_duplicate_decl_errors() {
    let dir = project("dupes", &[("src/main.hy", CLASS_SOURCE)]);
    let main = uri(&dir.join("src/main.hy"));
    let mut client = Client::spawn(&dir);
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": main, "languageId": "coil", "version": 1, "text": CLASS_SOURCE } }),
    );
    for version in 2..5 {
        let text = format!("{CLASS_SOURCE}{}", "\n".repeat(version));
        client.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": main, "version": version }, "contentChanges": [{ "text": text }] }),
        );
    }
    // A round-trip guarantees every notification above was processed.
    client.request("shutdown", Value::Null);
    let published = client.diagnostics_for(&main);
    assert!(!published.is_empty(), "no diagnostics published");
    for messages in published {
        assert!(messages.is_empty(), "unexpected diagnostics: {messages:?}");
    }
}

#[test]
fn imported_parse_error_is_reported_on_that_file() {
    let dir = project(
        "import-parse",
        &[
            ("src/main.hy", "use util::{helper};\n\nfn main() {\n    let _ = helper(1);\n}\n"),
            ("src/util.hy", "fn helper(int a) -> int {\n    return a +;\n}\n"),
        ],
    );
    let main = uri(&dir.join("src/main.hy"));
    let util = uri(&dir.join("src/util.hy"));
    let mut client = Client::spawn(&dir);
    let text = std::fs::read_to_string(dir.join("src/main.hy")).unwrap();
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": main, "languageId": "coil", "version": 1, "text": text } }),
    );
    client.request("shutdown", Value::Null);
    let util_diags = client.diagnostics_for(&util);
    assert!(
        util_diags.iter().any(|messages| !messages.is_empty()),
        "util.hy parse error not published: {util_diags:?}"
    );
    // No cascade ("non-class type", unknown function) in the importer.
    for messages in client.diagnostics_for(&main) {
        assert!(messages.is_empty(), "cascade in main.hy: {messages:?}");
    }
}
