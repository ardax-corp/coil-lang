// @expected E0102 — `join` returns the thread's own value type (#1905): a
// `Thread<int>` does not join as a `string`.
use thread::{join, spawn};

fn answer() -> int {
    return 42;
}

fn main() {
    let t = spawn(answer)?;
    let s: string = join(t)?;
}
