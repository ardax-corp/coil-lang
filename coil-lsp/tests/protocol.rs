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
        Self::spawn_with(root, &[])
    }

    /// Spawn with command-line flags (`--root`, host grants).
    fn spawn_with(root: &Path, args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_coil-lsp"))
            .args(args)
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

/// #581: classes and enums of a `.deps/<pkg>/src` module resolve. The
/// dependency file sits under both `.` and `.deps/shapes/src`; its
/// namespace must come from the innermost root (`shapes`).
#[test]
fn dependency_classes_and_enums_resolve() {
    let shapes = "class Box {\n    pub w: int,\n}\n\nimpl Box {\n    pub static fn make(int w) -> Box {\n        return new Box(w);\n    }\n\n    pub fn width() -> int {\n        return self.w;\n    }\n}\n\nenum Fault {\n    Bad { line: int },\n    Worse { line: int },\n}\n";
    let main = "use shapes::{Box, Fault};\n\nfn describe(Fault f) -> int {\n    return match f {\n        Fault::Bad { line } => line,\n        Fault::Worse { line } => line + 1,\n    };\n}\n\nfn main() {\n    let b = Box::make(3);\n    let _ = describe(Fault::Bad { line: b.width() });\n}\n";
    let dir = project(
        "dep-types",
        &[(".deps/shapes/src/shapes.hy", shapes), ("src/main.hy", main)],
    );
    let main_uri = uri(&dir.join("src/main.hy"));
    let mut client = Client::spawn(&dir);
    open(&mut client, &main_uri, main);
    client.request("shutdown", Value::Null);
    let published = client.diagnostics_for(&main_uri);
    assert!(!published.is_empty(), "no diagnostics published for main.hy");
    for messages in published {
        assert!(messages.is_empty(), "false diagnostics in main.hy: {messages:?}");
    }
}

/// #582: `--root` adds module search dirs and the grant flags reach the
/// checker, as for `coil compile`.
#[test]
fn command_line_roots_and_grants_reach_diagnostics() {
    let geo = "class Point {\n    pub x: int,\n}\n\nimpl Point {\n    pub static fn at(int x) -> Point {\n        return new Point(x);\n    }\n}\n";
    // FFI process-exec is gated at typecheck, so the editor sees it; the
    // host-call gates (`env::exec`, …) run after codegen and do not.
    let main = "use ffi::{declare};\nuse ffi::types::{Int, Ptr};\nuse geo::{Point};\n\nfn main() {\n    let p = Point::at(2);\n    let _ = declare(0, \"system\", (Ptr,), Int);\n    let _ = p.x;\n}\n";
    let dir = project("cli-roots", &[("vendor/geo.hy", geo), ("src/main.hy", main)]);
    let main_uri = uri(&dir.join("src/main.hy"));
    let last = |args: &[&str]| -> Vec<String> {
        let mut client = Client::spawn_with(&dir, args);
        open(&mut client, &main_uri, main);
        client.request("shutdown", Value::Null);
        client.diagnostics_for(&main_uri).pop().expect("diagnostics published")
    };

    let without = last(&[]);
    assert!(
        without.iter().any(|m| m.contains("Point")),
        "vendor/ is not a root by default: {without:?}"
    );
    assert!(
        without.iter().any(|m| m.contains("--allow-ffi-exec")),
        "ffi exec needs the grant: {without:?}"
    );

    let vendor = dir.join("vendor");
    let with = last(&["--root", vendor.to_str().unwrap(), "--allow-ffi-exec"]);
    assert!(with.is_empty(), "false diagnostics with flags: {with:?}");
}

/// #583: a reply already queued when stdin closes is still written, and
/// end of input is a clean exit. Exiting raced the writer thread before
/// (about 7% of runs lost the `initialize` result), so repeat.
#[test]
fn initialize_reply_survives_stdin_closing_at_once() {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "processId": null, "rootUri": null, "capabilities": {} },
    })
    .to_string();
    for run in 0..40 {
        let mut child = Command::new(env!("CARGO_BIN_EXE_coil-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn coil-lsp");
        let mut stdin = child.stdin.take().unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        drop(stdin);
        let output = child.wait_with_output().expect("wait");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("\"capabilities\""),
            "run {run}: initialize reply lost: {stdout:?}"
        );
        assert_eq!(output.status.code(), Some(0), "run {run}: EOF is a clean exit");
    }
}

fn open(client: &mut Client, uri: &str, text: &str) {
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "coil", "version": 1, "text": text } }),
    );
}

/// LSP position of the first `needle` in `text`, plus `delta` bytes.
fn position_of(text: &str, needle: &str, delta: usize) -> Value {
    let offset = text.find(needle).expect("needle") + delta;
    let line = text[..offset].matches('\n').count();
    let column = offset - text[..offset].rfind('\n').map_or(0, |i| i + 1);
    json!({ "line": line, "character": column })
}

#[test]
fn member_completion_lists_fields_and_methods() {
    let text = CLASS_SOURCE.replace("let _ = p.get();", "let _ = p.g");
    let dir = project("member-completion", &[("src/main.hy", &text)]);
    let main = uri(&dir.join("src/main.hy"));
    let mut client = Client::spawn(&dir);
    open(&mut client, &main, &text);
    let response = client.request(
        "textDocument/completion",
        json!({ "textDocument": { "uri": main }, "position": position_of(&text, "p.g", 3) }),
    );
    let labels: Vec<&str> = response["result"]
        .as_array()
        .expect("completion list")
        .iter()
        .map(|item| item["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["get"]);

    let empty = CLASS_SOURCE.replace("let _ = p.get();", "let _ = p.");
    client.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": main, "version": 2 }, "contentChanges": [{ "text": empty }] }),
    );
    let response = client.request(
        "textDocument/completion",
        json!({ "textDocument": { "uri": main }, "position": position_of(&empty, "p.", 2) }),
    );
    let labels: Vec<&str> = response["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["x", "get"]);
}

#[test]
fn member_goto_definition_finds_method_and_field() {
    let dir = project("member-goto", &[("src/main.hy", CLASS_SOURCE)]);
    let main = uri(&dir.join("src/main.hy"));
    let mut client = Client::spawn(&dir);
    open(&mut client, &main, CLASS_SOURCE);
    let response = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": main }, "position": position_of(CLASS_SOURCE, "p.get", 3) }),
    );
    assert_eq!(
        response["result"][0]["range"]["start"],
        position_of(CLASS_SOURCE, "get() -> int", 0)
    );
    let response = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": main }, "position": position_of(CLASS_SOURCE, "self.x", 5) }),
    );
    assert_eq!(response["result"][0]["range"]["start"], position_of(CLASS_SOURCE, "x: int", 0));
}

#[test]
fn inlay_hints_show_let_types_and_parameter_names() {
    let text = "fn area(int width, int height) -> int {\n    return width * height;\n}\n\nfn main() {\n    let height = 3;\n    let a = area(2, height);\n    let _ = a;\n}\n";
    let dir = project("inlay", &[("src/main.hy", text)]);
    let main = uri(&dir.join("src/main.hy"));
    let mut client = Client::spawn(&dir);
    open(&mut client, &main, text);
    let response = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": main },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 9, "character": 0 } },
        }),
    );
    let hints: Vec<(Value, &str)> = response["result"]
        .as_array()
        .expect("hint list")
        .iter()
        .map(|hint| (hint["position"].clone(), hint["label"].as_str().unwrap()))
        .collect();
    assert_eq!(
        hints,
        [
            (position_of(text, "height = 3", 6), ": int"),
            (position_of(text, "a = area", 1), ": int"),
            // `height` passed as `height` needs no hint.
            (position_of(text, "2, height", 0), "width:"),
        ]
    );
}

/// Code actions at `position` for the diagnostics last published on `uri`.
fn code_actions(client: &mut Client, uri: &str, position: Value) -> Vec<Value> {
    // Any round-trip collects the diagnostics published so far.
    client.request("coil/sync", Value::Null);
    let diagnostics = client
        .notifications
        .iter()
        .rev()
        .find(|n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == uri)
        .map(|n| n["params"]["diagnostics"].clone())
        .unwrap_or(json!([]));
    let response = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": { "start": position, "end": position },
            "context": { "diagnostics": diagnostics },
        }),
    );
    response["result"].as_array().expect("action list").clone()
}

/// The single edit of `action` on `uri`, applied to `text`.
fn apply_action(text: &str, uri: &str, action: &Value) -> String {
    let edits = action["edit"]["changes"][uri].as_array().expect("edits");
    assert_eq!(edits.len(), 1);
    let edit = &edits[0];
    let offset = |p: &Value| {
        let line = p["line"].as_u64().unwrap() as usize;
        let start: usize = text.split_inclusive('\n').take(line).map(str::len).sum();
        start + p["character"].as_u64().unwrap() as usize
    };
    let (start, end) = (offset(&edit["range"]["start"]), offset(&edit["range"]["end"]));
    format!("{}{}{}", &text[..start], edit["newText"].as_str().unwrap(), &text[end..])
}

#[test]
fn code_action_imports_unknown_function() {
    let main_text = "use util::{other};\n\nfn main() {\n    let _ = helper(1) + other();\n}\n";
    let dir = project(
        "action-import",
        &[
            ("src/main.hy", main_text),
            ("src/util.hy", "fn other() -> int {\n    return 2;\n}\n"),
            ("src/geo.hy", "fn helper(int a) -> int {\n    return a;\n}\n"),
        ],
    );
    let main = uri(&dir.join("src/main.hy"));
    let mut client = Client::spawn(&dir);
    open(&mut client, &main, main_text);
    let actions = code_actions(&mut client, &main, position_of(main_text, "helper", 0));
    let import = actions
        .iter()
        .find(|a| a["title"] == "Import `helper` from `geo`")
        .unwrap_or_else(|| panic!("no import action in {actions:?}"));
    assert_eq!(
        apply_action(main_text, &main, import),
        main_text.replace("{other};", "{other};\nuse geo::{helper};")
    );
}

#[test]
fn code_action_adds_default_arm_to_statement_match() {
    let text = "enum Color {\n    Red,\n    Green,\n}\n\nfn main() {\n    let c = Color::Red;\n    match c {\n        Color::Red => {},\n    };\n}\n";
    let dir = project("action-default", &[("src/main.hy", text)]);
    let main = uri(&dir.join("src/main.hy"));
    let mut client = Client::spawn(&dir);
    open(&mut client, &main, text);
    let actions = code_actions(&mut client, &main, position_of(text, "match c", 0));
    let fix = actions
        .iter()
        .find(|a| a["title"] == "Add `default =>` arm")
        .unwrap_or_else(|| panic!("no default-arm action in {actions:?}"));
    assert_eq!(
        apply_action(text, &main, fix),
        text.replace("{},\n    }", "{},\n        default => {},\n    }")
    );
}

#[test]
fn code_action_adds_inferred_type_annotation() {
    let text = "fn main() {\n    let total = 1 + 2;\n    let _ = total;\n}\n";
    let dir = project("action-annotate", &[("src/main.hy", text)]);
    let main = uri(&dir.join("src/main.hy"));
    let mut client = Client::spawn(&dir);
    open(&mut client, &main, text);
    let actions = code_actions(&mut client, &main, position_of(text, "total =", 2));
    assert_eq!(actions.len(), 1, "{actions:?}");
    assert_eq!(actions[0]["title"], "Add type annotation `: int`");
    assert_eq!(apply_action(text, &main, &actions[0]), text.replace("total =", "total: int ="));
}

#[test]
fn incremental_edits_are_applied_and_rechecked() {
    let text = "fn main() {\n    let a: int = 1;\n    let _ = a;\n}\n";
    let dir = project("incremental", &[("src/main.hy", text)]);
    let main = uri(&dir.join("src/main.hy"));
    let mut client = Client::spawn(&dir);
    open(&mut client, &main, text);
    // `1` → `"one"`, one keystroke-sized change after another.
    let at = |character: u32| json!({ "line": 1, "character": character });
    let edits = [
        (at(17), at(18), "\""),
        (at(18), at(18), "one"),
        (at(21), at(21), "\""),
    ];
    for (version, (start, end, new_text)) in edits.into_iter().enumerate() {
        client.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": main, "version": version + 2 },
                "contentChanges": [{ "range": { "start": start, "end": end }, "text": new_text }],
            }),
        );
    }
    let response = client.request(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": main }, "options": { "tabSize": 4, "insertSpaces": true } }),
    );
    let formatted = response["result"][0]["newText"].as_str().map(str::to_owned);
    let last = client.diagnostics_for(&main).pop().expect("diagnostics after edits");
    assert!(
        last.iter().any(|m| m.contains("int") && m.contains("string")),
        "expected a type mismatch, got {last:?} (buffer {formatted:?})"
    );
}

#[test]
fn code_action_expands_macros_and_diagnostics_see_generated_code() {
    let provider = "use macro::{TypeDecl, Code};\n\nderive Answer(TypeDecl t) -> Code {\n    return quote items {\n        impl ${t.name} {\n            pub static fn answer() -> int {\n                return 42;\n            }\n        }\n    };\n}\n";
    let main_text = "use gen::{Answer};\n\n#[derive(Answer)]\nclass C {\n    pub x: int,\n}\n\nfn main() {\n    let _ = C::answer();\n}\n";
    let dir = project(
        "action-expand",
        &[("src/main.hy", main_text), ("src/gen.hy", provider)],
    );
    let main = uri(&dir.join("src/main.hy"));
    let mut client = Client::spawn(&dir);
    open(&mut client, &main, main_text);
    // The generated `answer()` typechecks: no diagnostics on main.
    client.request("coil/sync", Value::Null);
    let diagnostics = client
        .notifications
        .iter()
        .rev()
        .find(|n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == main)
        .map(|n| n["params"]["diagnostics"].clone())
        .unwrap_or(json!([]));
    assert_eq!(diagnostics, json!([]), "{diagnostics}");
    let actions = code_actions(&mut client, &main, position_of(main_text, "#[derive", 0));
    let expand = actions
        .iter()
        .find(|a| a["title"] == "Expand macros in this file")
        .unwrap_or_else(|| panic!("no expand action in {actions:?}"));
    let expanded = apply_action(main_text, &main, expand);
    assert!(expanded.contains("pub static fn answer() -> int"), "{expanded}");
    assert!(!expanded.contains("#[derive(Answer)]"), "{expanded}");
}

#[test]
fn code_action_expands_function_macro_calls() {
    let provider = "use macro::{Expr, Code};\n\nmacro twice(Expr e) -> Code {\n    return quote expr { ${e} * 2 };\n}\n";
    let main_text = "use gen::{twice};\n\nfn main() {\n    let x = twice!(21);\n}\n";
    let dir = project(
        "action-expand-fn-macro",
        &[("src/main.hy", main_text), ("src/gen.hy", provider)],
    );
    let main = uri(&dir.join("src/main.hy"));
    let mut client = Client::spawn(&dir);
    open(&mut client, &main, main_text);
    client.request("coil/sync", Value::Null);
    let actions = code_actions(&mut client, &main, position_of(main_text, "twice!", 0));
    let expand = actions
        .iter()
        .find(|a| a["title"] == "Expand macros in this file")
        .unwrap_or_else(|| panic!("no expand action in {actions:?}"));
    let expanded = apply_action(main_text, &main, expand);
    assert!(expanded.contains("let x = 21 * 2;"), "{expanded}");
}
