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

/// 1-based line of `examples/fib.hy` holding `needle` (tests must not
/// hardcode line numbers: the example's header can change).
fn fib_line(needle: &str) -> usize {
    let src = std::fs::read_to_string(fib_entry()).expect("read examples/fib.hy");
    src.lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("`{needle}` not in examples/fib.hy"))
        + 1
}

/// One `#[test]` per case.
macro_rules! dap_tests {
    ($($case:ident => $test:ident;)*) => {
        $(
            #[test]
            fn $test() {
                $case();
            }
        )*
    };
}

/// A scratch directory for one case.
fn case_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{tag}-{}", std::process::id()))
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

fn dap_stop_on_entry_and_continue_case() {
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

fn dap_function_breakpoint_stack_and_locals_case() {
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

fn dap_line_breakpoint_hit_case() {
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
            "breakpoints": [{ "line": fib_line("if n <= 2") }, { "line": 99999 }]
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
    // `if n <= 2`: every statement carries a debug location.
    assert_eq!(
        bps[0].get("verified").and_then(|v| v.as_bool()),
        Some(true),
        "line 11 must verify: {set_bp}"
    );

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

fn dap_launch_compile_failure_case() {
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

fn dap_stop_on_entry_has_stack_and_step_case() {
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

fn dap_launch_allow_attach_grant_case() {
    let dir = case_dir("coil_dap_grant");
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

/// A panic is a `stopped` (reason `exception`) event with the stack intact;
/// resuming afterwards ends the session with exit code 1.
fn dap_panic_stops_for_inspection_case() {
    let dir = case_dir("coil-dap-panic");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let prog = dir.join("panics.hy");
    std::fs::write(
        &prog,
        "fn inner(int n) -> int {\n    if n > 2 {\n        panic \"boom\";\n    }\n    return n;\n}\n\nfn outer(int n) -> int {\n    return inner(n + 1) + 1;\n}\n\nfn main() {\n    let _ = outer(5);\n}\n",
    )
    .unwrap();
    let mut client = DapClient::spawn(&dir);
    initialize_and_launch(&mut client, prog.to_str().unwrap(), dir.to_str().unwrap(), false);
    let done = client.request("configurationDone", serde_json::json!({}));
    assert_eq!(done.get("success"), Some(&serde_json::json!(true)));
    let stopped = client.wait_for_event("stopped");
    assert_eq!(
        stopped.pointer("/body/reason").and_then(|v| v.as_str()),
        Some("exception"),
        "stopped={stopped}"
    );
    let stack = client.request("stackTrace", serde_json::json!({ "threadId": 1 }));
    let frames = stack
        .pointer("/body/stackFrames")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        frames.first().and_then(|f| f.get("name")).and_then(|n| n.as_str()) == Some("inner"),
        "top frame is the panicking fn: {stack}"
    );
    let _ = client.request("continue", serde_json::json!({ "threadId": 1 }));
    let exited = client.wait_for_event("exited");
    assert_eq!(exited.pointer("/body/exitCode"), Some(&serde_json::json!(1)));
    let _ = client.wait_for_event("terminated");
    client.disconnect();
}

fn dap_let_locals_at_line_breakpoint_case() {
    let dir = case_dir("coil-dap-locals");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let prog = dir.join("locals.hy");
    std::fs::write(
        &prog,
        "fn work(int n) -> int {\n    let doubled = n * 2;\n    let label = \"x\";\n    let total = doubled + 1;\n    if label != \"x\" {\n        panic \"label\";\n    }\n    return total;\n}\n\nfn main() {\n    let _ = work(20);\n}\n",
    )
    .unwrap();
    let mut client = DapClient::spawn(&dir);
    initialize_and_launch(&mut client, prog.to_str().unwrap(), dir.to_str().unwrap(), false);
    let set_bp = client.request(
        "setBreakpoints",
        serde_json::json!({
            "source": { "path": prog.to_string_lossy() },
            "breakpoints": [{ "line": 5 }]
        }),
    );
    assert_eq!(
        set_bp.pointer("/body/breakpoints/0/verified").and_then(|v| v.as_bool()),
        Some(true),
        "setBreakpoints={set_bp}"
    );
    let done = client.request("configurationDone", serde_json::json!({}));
    assert_eq!(done.get("success"), Some(&serde_json::json!(true)));
    let stopped = client.wait_for_event("stopped");
    assert_eq!(
        stopped.pointer("/body/reason").and_then(|v| v.as_str()),
        Some("breakpoint"),
        "stopped={stopped}"
    );
    let stack = client.request("stackTrace", serde_json::json!({ "threadId": 1 }));
    let frame_id = stack
        .pointer("/body/stackFrames/0/id")
        .and_then(|i| i.as_i64())
        .expect("frame id");
    let scopes = client.request("scopes", serde_json::json!({ "frameId": frame_id }));
    let variables_ref = scopes
        .pointer("/body/scopes/0/variablesReference")
        .and_then(|v| v.as_i64())
        .expect("variablesReference");
    let vars = client.request(
        "variables",
        serde_json::json!({ "variablesReference": variables_ref }),
    );
    let value = |name: &str| {
        vars.pointer("/body/variables")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .find(|v| v.get("name").and_then(|n| n.as_str()) == Some(name))
            .and_then(|v| v.get("value").and_then(|x| x.as_str()).map(str::to_string))
    };
    assert_eq!(value("n").as_deref(), Some("20"), "variables={vars}");
    assert_eq!(value("doubled").as_deref(), Some("40"), "variables={vars}");
    assert_eq!(value("total").as_deref(), Some("41"), "variables={vars}");
    assert!(value("label").is_some_and(|v| v.contains('x')), "variables={vars}");
    let clear = client.request(
        "setBreakpoints",
        serde_json::json!({ "source": { "path": prog.to_string_lossy() }, "breakpoints": [] }),
    );
    assert_eq!(clear.get("success"), Some(&serde_json::json!(true)));
    let _ = client.request("continue", serde_json::json!({ "threadId": 1 }));
    let _ = client.wait_for_event("terminated");
    client.disconnect();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Tasks show as threads: the running task is thread 1, suspended tasks are
/// listed with their state and their saved call chain.
fn dap_tasks_are_threads_case() {
    let dir = case_dir("coil-dap-tasks");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let prog = dir.join("tasks.hy");
    std::fs::write(
        &prog,
        "use task::{scope, Scope};\n\nfn napper() -> int {\n    task::sleep(50);\n    return 1;\n}\n\nfn worker() -> int {\n    let x = 7;\n    return x;\n}\n\nfn main() {\n    let _ = scope(fn (Scope s) {\n        s.spawn(fn () => napper());\n        s.spawn(fn () => worker());\n        0\n    });\n}\n",
    )
    .unwrap();
    let mut client = DapClient::spawn(&dir);
    initialize_and_launch(&mut client, prog.to_str().unwrap(), dir.to_str().unwrap(), false);
    let set_bp = client.request(
        "setBreakpoints",
        serde_json::json!({
            "source": { "path": prog.to_string_lossy() },
            "breakpoints": [{ "line": 10 }]
        }),
    );
    assert_eq!(
        set_bp.pointer("/body/breakpoints/0/verified").and_then(|v| v.as_bool()),
        Some(true),
        "setBreakpoints={set_bp}"
    );
    let _ = client.request("configurationDone", serde_json::json!({}));
    let stopped = client.wait_for_event("stopped");
    assert_eq!(stopped.pointer("/body/reason").and_then(|v| v.as_str()), Some("breakpoint"));

    let threads = client.request("threads", serde_json::json!({}));
    let list = threads.pointer("/body/threads").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let named = |id: i64| {
        list.iter()
            .find(|t| t.get("id").and_then(|i| i.as_i64()) == Some(id))
            .and_then(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
    };
    assert_eq!(list.len(), 3, "threads={threads}");
    assert_eq!(named(1).as_deref(), Some("task 2 [running]"), "threads={threads}");
    assert_eq!(named(1000).as_deref(), Some("main [blocked (end of scope)]"), "threads={threads}");
    assert_eq!(named(1001).as_deref(), Some("task 1 [blocked (sleep)]"), "threads={threads}");

    let frame_names = |thread: i64, client: &mut DapClient| -> Vec<String> {
        let stack = client.request("stackTrace", serde_json::json!({ "threadId": thread }));
        stack
            .pointer("/body/stackFrames")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect()
    };
    let running = frame_names(1, &mut client);
    assert_eq!(running.first().map(String::as_str), Some("worker"), "{running:?}");
    // The running task's chain ends at its task body, not in the root's frames.
    assert_eq!(running.last().map(String::as_str), Some("task::__task_body"), "{running:?}");
    let root = frame_names(1000, &mut client);
    assert!(root.iter().any(|n| n == "main"), "{root:?}");
    let napper = frame_names(1001, &mut client);
    assert!(napper.iter().any(|n| n == "napper"), "{napper:?}");

    let clear = client.request(
        "setBreakpoints",
        serde_json::json!({ "source": { "path": prog.to_string_lossy() }, "breakpoints": [] }),
    );
    assert_eq!(clear.get("success"), Some(&serde_json::json!(true)));
    let _ = client.request("continue", serde_json::json!({ "threadId": 1 }));
    let _ = client.wait_for_event("terminated");
    client.disconnect();
    let _ = std::fs::remove_dir_all(&dir);
}

dap_tests! {
    dap_stop_on_entry_and_continue_case => dap_stop_on_entry_and_continue;
    dap_function_breakpoint_stack_and_locals_case => dap_function_breakpoint_stack_and_locals;
    dap_line_breakpoint_hit_case => dap_line_breakpoint_hit;
    dap_launch_compile_failure_case => dap_launch_compile_failure;
    dap_stop_on_entry_has_stack_and_step_case => dap_stop_on_entry_has_stack_and_step;
    dap_launch_allow_attach_grant_case => dap_launch_allow_attach_grant;
    dap_panic_stops_for_inspection_case => dap_panic_stops_for_inspection;
    dap_let_locals_at_line_breakpoint_case => dap_let_locals_at_line_breakpoint;
    dap_tasks_are_threads_case => dap_tasks_are_threads;
}
