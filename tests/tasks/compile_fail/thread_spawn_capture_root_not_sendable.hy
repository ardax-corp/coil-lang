// @expected E0119 — a `gc::Root` captured by a spawned closure is not
// sendable, even when the closure also takes a sendable argument (#1199).
use thread::{spawn, join};
use gc::{root, Root};

fn main() {
    let r = root("x");
    let t = spawn(fn (int n) use (r) => n, 2)?;
    join(t)?;
}
