// Two nested multi-field record patterns in one arm each bind their own
// fields.
// @output 33
use io::{stdout, write};
use string::{format, to_bytes};

enum Inner {
    I { x: int, y: int },
}

enum Pair {
    P { a: Inner, b: Inner },
}

fn sum(Pair p) -> int {
    return match p {
        Pair::P { a: Inner::I { x: ax, y: ay }, b: Inner::I { x: bx, y: by } } => ax + ay + bx + by,
    };
}

fn main() {
    let p = Pair::P { a: Inner::I { x: 1, y: 2 }, b: Inner::I { x: 10, y: 20 } };
    let _ = write(stdout(), to_bytes(format("%i", sum(p))));
}
