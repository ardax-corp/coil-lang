// A cloned generic followed by a derived class: the clone must not leave the
// code generator's node cursor past the derived impls' nodes (#1190).
use io::{stdout, write};
use string::{format, to_bytes};

// Output: 7 5 Ver { n: 2 } true

fn spread<A: Num + Ord + Show>(A lo, A hi) -> string {
    let d = hi - lo;
    if d < lo {
        d = lo;
    }
    return show(d);
}

#[derive(Show, Eq)]
class Ver {
    pub n: int,
}

fn main() {
    let s = spread(3, 10);
    let t = spread(5, 6);
    let v = new Ver(2);
    write(stdout(), to_bytes(format("%s %s %s %v\n", s, t, show(v), v == new Ver(2))));
}
