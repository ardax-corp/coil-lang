// @expected E0119 — a `gc::Root` is not sendable, so it cannot be the value
// of a `thread::mutex` (the runtime used to report `NotSendable`).
use thread::mutex;
use gc::root;

fn main() {
    let _ = mutex(root("x"));
}
