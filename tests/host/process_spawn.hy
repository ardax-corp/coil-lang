// @needs processes
// @flags --allow-exec
// The `process` module: exit codes, captured output, pipes in both
// directions as streams, environment and working directory, signals,
// `try_wait`, and a program that does not exist.
use tests::assert;
use process::{Command, Stdio, ProcessError, exec};
use io::IoError;
use io::close;
use io::sync::read_to_end;
use io::sync::write_all;
use string::{from_bytes, to_bytes};

fn sh(string script) -> Command {
    let c = Command::new("sh");
    c.arg("-c");
    c.arg(script);
    return c;
}

fn out_of(Command c) -> string {
    return match c.output() {
        Result::Ok(o) => o.stdout,
        Result::Err(_) => "<failed>",
    };
}

test("exec returns the exit code") {
    assert!(exec("sh", ["-c", "exit 0"]).unwrap_or(-1) == 0);
    assert!(exec("sh", ["-c", "exit 7"]).unwrap_or(-1) == 7);
}

test("output collects stdout, stderr and the status") {
    let o = sh("printf out; printf err >&2; exit 4").output()?;
    assert!(o.status == 4);
    assert!(o.stdout == "out");
    assert!(o.stderr == "err");
}

test("arguments reach the program unchanged, without a shell") {
    let c = Command::new("printf");
    c.args(["%s|%s", "a b"]);
    c.arg("$HOME");
    assert!(out_of(c) == "a b|$HOME");
}

test("env, env_remove, env_clear and cwd shape the child") {
    let c = sh("printf '%s:%s' \"$COIL_PROC_T\" \"$(pwd)\"");
    c.env("COIL_PROC_T", "set");
    c.cwd("/");
    assert!(out_of(c) == "set:/");
    let gone = sh("printf '[%s]' \"$COIL_PROC_T\"");
    gone.env("COIL_PROC_T", "x");
    gone.env_remove("COIL_PROC_T");
    assert!(out_of(gone) == "[]");
    let empty = Command::new("/bin/sh");
    empty.args(["-c", "printf '[%s]' \"$HOME\""]);
    empty.env_clear();
    assert!(out_of(empty) == "[]");
}

test("one command starts any number of children") {
    let c = sh("printf again");
    assert!(out_of(c) == "again");
    assert!(out_of(c) == "again");
}

test("piped stdin and stdout are streams") {
    let c = Command::new("tr");
    c.args(["a-z", "A-Z"]);
    c.stdin(Stdio::Pipe);
    c.stdout(Stdio::Pipe);
    let child = c.spawn()?;
    let w = child.stdin() ?? panic "stdin was piped";
    assert!(child.stdin().is_none());
    write_all(w, to_bytes("shout\n"))?;
    close(w)?;
    let r = child.stdout() ?? panic "stdout was piped";
    assert!(from_bytes(read_to_end(r)?).unwrap_or("") == "SHOUT\n");
    assert!(child.wait()? == 0);
    assert!(child.wait()? == 0);
}

test("a stream that is not piped has no pipe") {
    let c = Command::new("true");
    c.stdout(Stdio::Null);
    let child = c.spawn()?;
    assert!(child.stdout().is_none());
    assert!(child.stdin().is_none());
    assert!(child.pid() > 0);
    assert!(child.wait()? == 0);
}

test("output with stdin piped but not written sees end of input") {
    let c = Command::new("cat");
    c.stdin(Stdio::Pipe);
    let o = c.output()?;
    assert!(o.status == 0);
    assert!(o.stdout == "");
}

test("try_wait, signal and kill") {
    let c = Command::new("sleep");
    c.arg("5");
    let child = c.spawn()?;
    let running = child.try_wait()?;
    assert!(running.is_none());
    child.signal(15)?;
    assert!(child.wait()? == 128 + 15);
    let bad = match child.signal(15) {
        Result::Err(ProcessError::Io(IoError::InvalidInput)) => true,
        default => false,
    };
    assert!(bad);
    let other = c.spawn()?;
    other.kill()?;
    assert!(other.wait()? == 128 + 9);
    other.kill()?;
}

test("a missing program fails to spawn with NotFound") {
    let missing = match exec("/nonexistent/coil-no-such-program", []) {
        Result::Err(ProcessError::Spawn(IoError::NotFound)) => true,
        default => false,
    };
    assert!(missing);
}
