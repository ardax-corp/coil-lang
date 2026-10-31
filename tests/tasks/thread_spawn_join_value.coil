// A user `thread::spawn` copies its arguments to the new thread and `join`
// returns the worker's value.
use tests::assert;
use thread::{join, spawn};

fn add1(int n) -> int {
    return n + 1;
}

test("spawn then join returns the worker's value") {
    let t = spawn(add1, 41)?;
    let v = join(t)?;
    assert!(v == 42);
}
