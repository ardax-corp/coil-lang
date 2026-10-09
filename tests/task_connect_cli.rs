//! Task connects past the listen backlog (coil-lang#832), through `coil test`.
//!
//! Unix and Windows: a connect suspends just its task there (see
//! `docs/internals/tasks.md`).
#![cfg(any(unix, windows))]

use std::path::PathBuf;
use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

/// `coil test` re-execs `coil-test`; `cargo test -p coil` alone does not build it.
fn ensure_coil_test() {
    let coil = PathBuf::from(coil_bin());
    let helper = coil_cli::sibling_bin(&coil, "coil-test");
    if helper.is_file() {
        return;
    }
    let status = Command::new("cargo")
        .args(["build", "-q", "-p", "coil-test"])
        .status()
        .expect("spawn cargo build -p coil-test");
    assert!(
        status.success() && helper.is_file(),
        "coil-test missing at {}",
        helper.display()
    );
}

/// 200 client tasks against a 128-entry backlog: the ones whose SYN is
/// dropped wait in their own task while the server task keeps accepting.
/// Counts connections only: a full SYN queue can make the kernel drop a
/// client's data on loopback (syncookies).
const BACKLOG_TEST: &str = r#"use task::{scope, Scope, TaskError};
use io::close;
use io::Stream;
use io::net::tcp::connect;
use io::net::tcp::listen;
use io::net::tcp::local_addr;
use io::sync::accept_wait;

fn connect_close(int port) -> int {
    let c = match connect("127.0.0.1", port) {
        Result::Ok(c) => c,
        Result::Err(_) => panic "connect",
    };
    let _ = close(c);
    return 0;
}

fn accepted(Result<int, TaskError> r) -> int {
    return match r {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
}

test("connects past the listen backlog suspend only their own task") {
    let listener = listen("127.0.0.1", 0)?;
    let port = local_addr(listener)?[1];
    let r = scope(
        fn (Scope s) use (listener, port) {
            let server = s
                .spawn(
                    fn () use (listener) {
                        let n = 0;
                        while n < 200 {
                            let c = match accept_wait(listener) {
                                Result::Ok(c) => c,
                                Result::Err(_) => panic "accept",
                            };
                            let _ = close(c);
                            n = n + 1;
                        }
                        n
                    },
                );
            let i = 0;
            while i < 200 {
                s.spawn(fn () use (port) => connect_close(port));
                i = i + 1;
            }
            accepted(server.join())
        },
    );
    assert(accepted(r) == 200)?;
}
"#;

#[test]
fn task_connects_past_the_listen_backlog_finish() {
    ensure_coil_test();
    let dir = std::env::temp_dir().join(format!("coil_task_connect_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("positive")).expect("temp dir");
    std::fs::write(dir.join("positive/backlog.hy"), BACKLOG_TEST).expect("write test");
    let mut cmd = Command::new(coil_bin());
    cmd.args(["test", "-j", "1", "--allow-net"]);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    let out = cmd.arg(&dir).output().expect("spawn coil test");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
