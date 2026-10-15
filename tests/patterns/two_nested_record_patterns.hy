// Two nested multifield records in one outer arm — each must unpack into
// distinct scratch regions so the second nested payload cannot overwrite
// the first nested bindings.
// both nested multifield bindings must survive
// Output: 33
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
    write(stdout(), to_bytes(format("%i", sum(p))));
}
