// @needs processes unix
// @flags --allow-exec
// The parts of the `process` module that are Unix-shaped: a cleared
// environment, the working directory as a Unix path, and signals with
// their `128 + signal` exit status.
use tests::assert;
use process::{Command, ProcessError};
use io::IoError;

fn out_of(Command c) -> string {
    return match c.output() {
        Result::Ok(o) => o.stdout,
        Result::Err(_) => "<failed>",
    };
}

test("cwd and env_clear shape the child") {
    let c = Command::new("/bin/sh");
    c.args(["-c", "pwd"]);
    c.cwd("/");
    assert!(out_of(c) == "/\n");
    let empty = Command::new("/bin/sh");
    empty.args(["-c", "printf '[%s]' \"$HOME\""]);
    empty.env_clear();
    assert!(out_of(empty) == "[]");
}

test("signal and kill report 128 plus the signal") {
    let c = Command::new("sleep");
    c.arg("5");
    let child = c.spawn()?;
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
}
