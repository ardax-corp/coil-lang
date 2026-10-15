// Tiny direct-call inlining must preserve call semantics end-to-end.
// Output: 7
use io::{stdout, write};
use string::{format, to_bytes};

fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    write(stdout(), to_bytes(format("%i", add(3, 4))));
}
