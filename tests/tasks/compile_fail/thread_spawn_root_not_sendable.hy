// @expected E0119 — a `gc::Root` is not sendable, so it cannot be a
// `thread::spawn` argument.
use thread::{spawn, join};
use gc::{root, Root};

fn hold(Root<string> r) -> int {
    return 0;
}

fn main() {
    let r = root("x");
    let t = spawn(hold, r)?;
    join(t)?;
}
