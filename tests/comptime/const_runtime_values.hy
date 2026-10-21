// A `const` whose value exists only while a program runs (a function value,
// a standard stream) keeps its startup initializer instead of E0134.
// Output: hi
use tests::assert;
use io::{stdout, write};
use string::to_bytes;

class Holder {
    pub f: fn(int) -> int,
}

fn twice(int x) -> int {
    return x * 2;
}

fn make() -> Holder {
    return new Holder(twice);
}

const H = make();

const F = twice;

const OUT = stdout();

fn main() {
    let f = H.f;
    assert!(f(4) == 8);
    assert!(F(5) == 10);
    write(OUT, to_bytes("hi"));
}
