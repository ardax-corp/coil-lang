// Let-bound packs exercise the typed-lookup codegen path (not the literal
// Tuple/Array shortcuts). Wrong Index expansion would silently mis-bind.
// Output: 6
use io::{stdout, write};
use string::{format, to_bytes};

fn add3(int a, int b, int c) -> int {
    return a + b + c;
}

fn main() {
    let pack = (1, 2, 3);
    write(stdout(), to_bytes(format("%i", add3(...pack))));
}
