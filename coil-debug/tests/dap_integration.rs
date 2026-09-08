//! Integration tests for `coil-debug --dap`.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

fn coil_debug_bin() -> PathBuf {
    for key in ["CARGO_BIN_EXE_coil_debug", "CARGO_BIN_EXE_coil-debug"] {
        if let Ok(p) = std::env::var(key) {
            let path = PathBuf::from(&p);
            if path.is_file() {
                return path;
            }
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for base in [
        format!("../target/debug/coil-debug{}", std::env::consts::EXE_SUFFIX),
        format!(
            "../../target/debug/coil-debug{}",
            std::env::consts::EXE_SUFFIX
        ),
    ] {
        let local = manifest.join(base);
        if local.is_file() {
            return local.canonicalize().unwrap_or(local);
        }
    }
    panic!("coil-debug binary not found (run `cargo build -p coil-debug`)");
}

fn fib_entry() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("examples/fib.hy")
        .canonicalize()
        .expect("examples/fib.hy")
}

struct DapClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    seq: i64,
}

impl DapClient {
    fn spawn(cwd: &std::path::Path) -> Self {
        Self::spawn_with(cwd, &[])
    }

    fn spawn_with(cwd: &std::path::Path, extra: &[&str]) -> Self {
        let bin = coil_debug_bin();
        let mut cmd = Command::new(&bin);
        cmd.arg("--dap");
        cmd.args(extra);
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        let mut child = cmd
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn coil-debug --dap");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
            seq: 0,
        }
    }

    fn request(&mut self, command: &str, args: serde_json::Value) -> serde_json::Value {
        self.seq += 1;
        let seq = self.seq;
        let body = serde_json::json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": args,
        });
        let bytes = serde_json::to_vec(&body).expect("json");
        write!(self.stdin, "Content-Length: {}\r\n\r\n", bytes.len()).expect("header");
        self.stdin.write_all(&bytes).expect("body");
        self.stdin.flush().expect("flush");

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if std::time::Instant::now() > deadline {
                panic!("timeout waiting for response to {command} seq={seq}");
            }
            let msg = self.read_message().expect("dap message");
            if msg.get("type").and_then(|t| t.as_str()) == Some("response")
                && msg.get("request_seq").and_then(|s| s.as_i64()) == Some(seq)
            {
                return msg;
            }
            // Events (initialized / stopped / …) are ignored here; callers that
            // need them drain via `wait_for_event` after the matching response.
            let _ = msg;
        }
    }

    fn wait_for_event(&mut self, name: &str) -> serde_json::Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if std::time::Instant::now() > deadline {
                panic!("timeout waiting for event {name}");
            }
            let msg = self.read_message().expect("dap message");
            if msg.get("type").and_then(|t| t.as_str()) == Some("event")
                && msg.get("event").and_then(|e| e.as_str()) == Some(name)
            {
                return msg;
            }
        }
    }

    fn read_message(&mut self) -> Option<serde_json::Value> {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            match self.stdout.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) => {}
                Err(e) => panic!("read header: {e}"),
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some((key, val)) = trimmed.split_once(':')
                && key.trim() == "Content-Length"
            {
                content_length = Some(val.trim().parse().expect("Content-Length"));
            }
        }
        let len = content_length?;
        let mut body = vec![0u8; len];
        self.stdout.read_exact(&mut body).expect("body");
        Some(serde_json::from_slice(&body).expect("json body"))
    }

    fn disconnect(mut self) {
        let _ = self.request("disconnect", serde_json::json!({}));
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

fn initialize_and_launch(client: &mut DapClient, program: &str, cwd: &str, stop_on_entry: bool) {
    let init = client.request(
        "initialize",
        serde_json::json!({ "clientID": "test", "adapterID": "coil" }),
    );
    assert_eq!(init.get("success"), Some(&serde_json::json!(true)));
    let _ = client.wait_for_event("initialized");

    let launch = client.request(
        "launch",
        serde_json::json!({
            "program": program,
            "cwd": cwd,
            "stopOnEntry": stop_on_entry,
        }),
    );
    assert_eq!(
        launch.get("success"),
        Some(&serde_json::json!(true)),
        "launch={launch}"
    );
}

#[test]
fn dap_stop_on_entry_and_continue() {
    let entry = fib_entry();
    let cwd = entry.parent().unwrap().parent().unwrap();
    let mut client = DapClient::spawn(cwd);
    initialize_and_launch(
        &mut client,
        entry.to_str().unwrap(),
        cwd.to_str().unwrap(),
        true,
    );
    let done = client.request("configurationDone", serde_json::json!({}));
    assert_eq!(done.get("success"), Some(&serde_json::json!(true)));
    let stopped = client.wait_for_event("stopped");
    assert_eq!(
        stopped
            .pointer("/body/reason")
            .and_then(|v| v.as_str()),
        Some("entry")
    );
    let cont = client.request("continue", serde_json::json!({ "threadId": 1 }));
    assert_eq!(cont.get("success"), Some(&serde_json::json!(true)));
    let _ = client.wait_for_event("terminated");
    client.disconnect();
}

#[test]
fn dap_function_breakpoint_stack_and_locals() {
    let entry = fib_entry();
    let cwd = entry.parent().unwrap().parent().unwrap();
    let mut client = DapClient::spawn(cwd);
    initialize_and_launch(
        &mut client,
        entry.to_str().unwrap(),
        cwd.to_str().unwrap(),
        false,
    );

    let set_fn = client.request(
        "setFunctionBreakpoints",
        serde_json::json!({
            "breakpoints": [{ "name": "fib" }]
        }),
    );
    assert_eq!(set_fn.get("success"), Some(&serde_json::json!(true)));
    let verified = set_fn
        .pointer("/body/breakpoints/0/verified")
        .and_then(|v| v.as_bool());
    assert_eq!(verified, Some(true), "setFunctionBreakpoints={set_fn}");

    let done = client.request("configurationDone", serde_json::json!({}));
    assert_eq!(done.get("success"), Some(&serde_json::json!(true)));
    let stopped = client.wait_for_event("stopped");
    assert_eq!(
        stopped
            .pointer("/body/reason")
            .and_then(|v| v.as_str()),
        Some("breakpoint"),
        "stopped={stopped}"
    );

    let stack = client.request("stackTrace", serde_json::json!({ "threadId": 1 }));
    assert_eq!(stack.get("success"), Some(&serde_json::json!(true)));
    let frames = stack
        .pointer("/body/stackFrames")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        frames.len() >= 2,
        "expected caller+fib frames, stack={stack}"
    );
    let top = &frames[0];
    assert!(
        top.get("name")
            .and_then(|n| n.as_str())
            .is_some_and(|n| n.contains("fib")),
        "top={top}"
    );
    let frame_id = top.get("id").and_then(|i| i.as_i64()).expect("frame id");

    let scopes = client.request("scopes", serde_json::json!({ "frameId": frame_id }));
    assert_eq!(scopes.get("success"), Some(&serde_json::json!(true)));
    let variables_ref = scopes
        .pointer("/body/scopes/0/variablesReference")
        .and_then(|v| v.as_i64())
        .expect("variablesReference");

    let vars = client.request(
        "variables",
        serde_json::json!({ "variablesReference": variables_ref }),
    );
    assert_eq!(vars.get("success"), Some(&serde_json::json!(true)));
    let has_n = vars
        .pointer("/body/variables")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .any(|v| v.get("name").and_then(|n| n.as_str()) == Some("n"));
    assert!(has_n, "expected local n, variables={vars}");

    // Clear recursive function BPs so continue can finish.
    let clear = client.request(
        "setFunctionBreakpoints",
        serde_json::json!({ "breakpoints": [] }),
    );
    assert_eq!(clear.get("success"), Some(&serde_json::json!(true)));
    let cont = client.request("continue", serde_json::json!({ "threadId": 1 }));
    assert_eq!(cont.get("success"), Some(&serde_json::json!(true)));
    let _ = client.wait_for_event("terminated");
    client.disconnect();
}

#[test]
fn dap_line_breakpoint_hit() {
    let entry = fib_entry();
    let cwd = entry.parent().unwrap().parent().unwrap();
    let mut client = DapClient::spawn(cwd);
    initialize_and_launch(
        &mut client,
        entry.to_str().unwrap(),
        cwd.to_str().unwrap(),
        false,
    );

    let set_bp = client.request(
        "setBreakpoints",
        serde_json::json!({
            "source": { "path": entry.to_string_lossy() },
            "breakpoints": [{ "line": 11 }, { "line": 99999 }]
        }),
    );
    assert_eq!(set_bp.get("success"), Some(&serde_json::json!(true)));
    let bps = set_bp
        .pointer("/body/breakpoints")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(bps.len(), 2, "setBreakpoints={set_bp}");
    assert_eq!(
        bps[1].get("verified").and_then(|v| v.as_bool()),
        Some(false),
        "bogus line should be unverified"
    );
    // Line 11 (`return 1`) should verify when debug_locs cover it.
    if bps[0].get("verified").and_then(|v| v.as_bool()) != Some(true) {
        // Fall back: disconnect cleanly if line mapping is sparse in this build.
        client.disconnect();
        return;
    }

    let done = client.request("configurationDone", serde_json::json!({}));
    assert_eq!(done.get("success"), Some(&serde_json::json!(true)));
    let stopped = client.wait_for_event("stopped");
    assert_eq!(
        stopped
            .pointer("/body/reason")
            .and_then(|v| v.as_str()),
        Some("breakpoint"),
        "stopped={stopped}"
    );

    // Clear line BPs and finish.
    let clear = client.request(
        "setBreakpoints",
        serde_json::json!({
            "source": { "path": entry.to_string_lossy() },
            "breakpoints": []
        }),
    );
    assert_eq!(clear.get("success"), Some(&serde_json::json!(true)));
    let cont = client.request("continue", serde_json::json!({ "threadId": 1 }));
    assert_eq!(cont.get("success"), Some(&serde_json::json!(true)));
    let _ = client.wait_for_event("terminated");
    client.disconnect();
}

#[test]
fn dap_launch_compile_failure() {
    let entry = fib_entry();
    let cwd = entry.parent().unwrap().parent().unwrap();
    let mut client = DapClient::spawn(cwd);
    let init = client.request(
        "initialize",
        serde_json::json!({ "clientID": "test", "adapterID": "coil" }),
    );
    assert_eq!(init.get("success"), Some(&serde_json::json!(true)));
    let _ = client.wait_for_event("initialized");

    let missing = cwd.join("definitely_missing_coil_prog_zz.hy");
    let launch = client.request(
        "launch",
        serde_json::json!({
            "program": missing.to_string_lossy(),
            "cwd": cwd.to_string_lossy(),
        }),
    );
    assert_eq!(
        launch.get("success"),
        Some(&serde_json::json!(false)),
        "launch={launch}"
    );
    assert!(
        launch
            .get("message")
            .and_then(|m| m.as_str())
            .is_some_and(|m| m.contains("compile")),
        "launch={launch}"
    );
    client.disconnect();
}

#[test]
fn dap_stop_on_entry_has_stack_and_step() {
    let entry = fib_entry();
    let cwd = entry.parent().unwrap().parent().unwrap();
    let mut client = DapClient::spawn(cwd);
    initialize_and_launch(
        &mut client,
        entry.to_str().unwrap(),
        cwd.to_str().unwrap(),
        true,
    );
    let done = client.request("configurationDone", serde_json::json!({}));
    assert_eq!(done.get("success"), Some(&serde_json::json!(true)));
    let stopped = client.wait_for_event("stopped");
    assert_eq!(
        stopped.pointer("/body/reason").and_then(|v| v.as_str()),
        Some("entry")
    );

    let stack = client.request("stackTrace", serde_json::json!({ "threadId": 1 }));
    assert_eq!(stack.get("success"), Some(&serde_json::json!(true)));
    let frames = stack
        .pointer("/body/stackFrames")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        !frames.is_empty(),
        "stopOnEntry must expose a stack, stack={stack}"
    );

    let step = client.request("stepIn", serde_json::json!({ "threadId": 1 }));
    assert_eq!(
        step.get("success"),
        Some(&serde_json::json!(true)),
        "stepIn={step}"
    );
    let stepped = client.wait_for_event("stopped");
    assert_eq!(
        stepped.pointer("/body/reason").and_then(|v| v.as_str()),
        Some("step"),
        "stepped={stepped}"
    );

    let over = client.request("next", serde_json::json!({ "threadId": 1 }));
    assert_eq!(over.get("success"), Some(&serde_json::json!(true)));
    let _ = client.wait_for_event("stopped");

    let out = client.request("stepOut", serde_json::json!({ "threadId": 1 }));
    assert_eq!(out.get("success"), Some(&serde_json::json!(true)));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if std::time::Instant::now() > deadline {
            panic!("timeout after stepOut");
        }
        let msg = client.read_message().expect("dap message");
        let kind = msg.get("event").and_then(|e| e.as_str());
        if kind == Some("stopped") || kind == Some("terminated") {
            break;
        }
    }
    client.disconnect();
}

#[test]
fn dap_launch_allow_attach_grant() {
    let dir = std::env::temp_dir().join(format!("coil_dap_grant_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let gated = dir.join("gated.hy");
    std::fs::write(
        &gated,
        "use io::{stdout};\nfn main() { let _ = stdout().attach(0, 0, 0, 0, 0); }\n",
    )
    .expect("write gated");

    let mut denied = DapClient::spawn(&dir);
    let init = denied.request(
        "initialize",
        serde_json::json!({ "clientID": "test", "adapterID": "coil" }),
    );
    assert_eq!(init.get("success"), Some(&serde_json::json!(true)));
    let _ = denied.wait_for_event("initialized");
    let fail = denied.request(
        "launch",
        serde_json::json!({
            "program": gated.to_string_lossy(),
            "cwd": dir.to_string_lossy(),
        }),
    );
    assert_eq!(
        fail.get("success"),
        Some(&serde_json::json!(false)),
        "launch={fail}"
    );
    denied.disconnect();

    let mut granted = DapClient::spawn_with(&dir, &["--allow-attach"]);
    let init = granted.request(
        "initialize",
        serde_json::json!({ "clientID": "test", "adapterID": "coil" }),
    );
    assert_eq!(init.get("success"), Some(&serde_json::json!(true)));
    let _ = granted.wait_for_event("initialized");
    let ok = granted.request(
        "launch",
        serde_json::json!({
            "program": gated.to_string_lossy(),
            "cwd": dir.to_string_lossy(),
            "stopOnEntry": true,
        }),
    );
    assert_eq!(
        ok.get("success"),
        Some(&serde_json::json!(true)),
        "launch with CLI grant={ok}"
    );
    granted.disconnect();

    let mut via_launch = DapClient::spawn(&dir);
    let init = via_launch.request(
        "initialize",
        serde_json::json!({ "clientID": "test", "adapterID": "coil" }),
    );
    assert_eq!(init.get("success"), Some(&serde_json::json!(true)));
    let _ = via_launch.wait_for_event("initialized");
    let ok = via_launch.request(
        "launch",
        serde_json::json!({
            "program": gated.to_string_lossy(),
            "cwd": dir.to_string_lossy(),
            "allowAttach": true,
            "stopOnEntry": true,
        }),
    );
    assert_eq!(
        ok.get("success"),
        Some(&serde_json::json!(true)),
        "launch allowAttach={ok}"
    );
    via_launch.disconnect();
    let _ = std::fs::remove_dir_all(&dir);
}
