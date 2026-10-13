// Prefix args + spread pack must flatten in source order. A buggy flatten that
// drops the prefix or reverses pack elements would still typecheck for
// `add3(int,int,int)` but print the wrong sum.
// Output: 6
use io::{stdout, write};
use string::{format, to_bytes};

fn add3(int a, int b, int c) -> int {
    return a + b + c;
}

fn main() {
    write(stdout(), to_bytes(format("%i", add3(1, ...(2, 3)))));
}
