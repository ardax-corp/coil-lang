// @expected E0119 — a `Thread` handle is not sendable, so it cannot be the
// value of a `thread::rwlock`.
use thread::{rwlock, spawn};

fn noop() -> int {
    return 0;
}

fn main() {
    let t = spawn(noop)?;
    let _ = rwlock(t);
}
