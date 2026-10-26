// Threads: a shared counter behind a `mutex`.
//
// Lock calls fail with a `LockError` and `spawn` / `join` with a
// `ThreadError`; `count` keeps the thread calls and their `?`s together.
//
// @output 2

use thread::{Mutex, ThreadError, join, mutex, spawn, with_lock};
use io::sync::print;
use string::format;

fn bump(Mutex m) {
    with_lock(m, fn (int n) => (n + 1, 0))?;
}

fn count(Mutex m) -> Result<unit, ThreadError> {
    let t1 = spawn(bump, m)?;
    let t2 = spawn(bump, m)?;
    let _ = join(t1)?;
    let _ = join(t2)?;
    return Ok(());
}

fn main() {
    let m = mutex(0)?;
    match count(m) {
        Ok(_) => {},
        Err(e) => panic format("threads failed: %v", e),
    }
    let n = with_lock(m, fn (int x) => (x, x))?;
    let _ = print(format("%i", n));
}
