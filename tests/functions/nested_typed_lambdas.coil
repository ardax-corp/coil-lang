// Lambdas with typed parameters nested inside other lambdas see their own
// parameters and compute the right result.
// @output 42
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let add = fn (int x) => fn (int y) use (x) => x + y;
    let add40 = add(40);
    let _ = write(stdout(), to_bytes(format("%i", add40(2))));
}
