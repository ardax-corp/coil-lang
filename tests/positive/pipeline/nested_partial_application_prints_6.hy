// Nested partials (`f(1)` → `p(2)` → `q(3)`) must merge filled masks without
// clobbering earlier holes — a common ObjFn regression.
// Output: 6
use io::{stdout, write};
use string::{format, to_bytes};

fn add3(int a, int b, int c) -> int {
    return a + b + c;
}

fn main() {
    let p = add3(1);
    let q = p(2);
    write(stdout(), to_bytes(format("%i", q(3))));
}
