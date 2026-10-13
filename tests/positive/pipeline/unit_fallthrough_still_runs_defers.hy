// unit epilogue must still emit defers before CONST 0; RETURN
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
