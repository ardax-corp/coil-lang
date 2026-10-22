// A `defer` runs when a unit function falls off the end of its body, after
// the body's own statements.
// @output bd
use io::{stdout, write};
use string::{format, to_bytes};

fn f() {
    defer {
        let _ = write(stdout(), to_bytes("d"));
    }
    let _ = write(stdout(), to_bytes("b"));
}

fn main() {
    f();
}
