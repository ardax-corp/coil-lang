// #1684: a static holding `stdout()`, `stderr()` or `stdin()` works in a
// `thread::spawn` worker. The standard streams are process-wide, so the
// worker opens its own handle to the same stream instead of reading null.
// @output worker 7\nwrote 9\nerr ok\nin ok\nclosed ok\nback in main\n
use io::{close, stderr, stdin, stdout, write, IoError, Stream};
use string::{format, to_bytes};
use thread::{join, spawn};

const OUT = stdout();

const ERR: Stream = stderr();

static let IN: Stream = stdin();

static let SHUT: Stream = stdout();

fn say(int i) -> int {
    return match write(OUT, to_bytes(format("worker %i\n", i))) {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
}

fn err_writes() -> bool {
    return match write(ERR, to_bytes("")) {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
}

fn in_has_fd() -> bool {
    return match IN.fd() {
        Result::Ok(fd) => fd >= 0,
        Result::Err(_) => false,
    };
}

// The spawner closed SHUT before spawning: the worker's copy is closed too.
fn shut_is_closed() -> bool {
    return match write(SHUT, to_bytes("not printed\n")) {
        Result::Err(IoError::AlreadyClosed) => true,
        default => false,
    };
}

fn run_int(int -> int f, int arg) -> int {
    let t = match spawn(f, arg) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "spawn failed",
    };
    return match join(t) {
        Result::Ok(n) => n,
        Result::Err(_) => -2,
    };
}

fn run_bool(() -> bool f) -> bool {
    let t = match spawn(f) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "spawn failed",
    };
    return match join(t) {
        Result::Ok(b) => b,
        Result::Err(_) => false,
    };
}

fn check(string what, bool ok) {
    if ok {
        let _ = write(OUT, to_bytes(format("%s ok\n", what)));
    } else {
        let _ = write(OUT, to_bytes(format("%s FAILED\n", what)));
    }
}

fn main() {
    let n = run_int(say, 7);
    let _ = write(OUT, to_bytes(format("wrote %i\n", n)));
    check("err", run_bool(err_writes));
    check("in", run_bool(in_has_fd));
    let _ = close(SHUT);
    check("closed", run_bool(shut_is_closed));
    let _ = write(OUT, to_bytes("back in main\n"));
}
