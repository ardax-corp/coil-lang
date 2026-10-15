// A `defer` runs when a unit function falls off the end of its body, after
// the body's own statements.
// Output: bd
use io::{stdout, write};
use string::{format, to_bytes};

fn f() {
    defer {
        write(stdout(), to_bytes("d"));
    }
    write(stdout(), to_bytes("b"));
}

fn main() {
    f();
}
