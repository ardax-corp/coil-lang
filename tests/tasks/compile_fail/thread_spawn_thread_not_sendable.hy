// @expected E0119 — a `Thread` handle is not sendable, so it cannot be a
// `thread::spawn` argument.
use thread::{spawn, Thread};

fn noop() -> int {
    return 0;
}

fn work(Thread t) -> int {
    return 1;
}

fn main() {
    let t0 = spawn(noop)?;
    let t = spawn(work, t0);
}
