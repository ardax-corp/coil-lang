// A `readonly class` compared structurally, a `readonly` array passed to a reader, a local `const` and a mutating method.
// @output 3 6 true 1 2\n
use io::sync::println;
use string::format;

readonly class Pt {
    pub x: int,
    pub y: int,
}

class Acc {
    pub n: int,
}

impl Acc {
    pub fn add(int k) {
        self.n += k;
    }
}

fn sum_ro(readonly [int; 3] v) -> int {
    let t = 0;
    for x in v {
        t += x;
    }
    return t;
}

fn main() {
    const K = 3;
    let xs = readonly [1, 2, 3];
    let p = new Pt(1, 2);
    let p2 = new Pt(1, 2);
    let a = new Acc(0);
    a.add(1);
    let _ = println(format("%i %i %z %i %i", K, sum_ro(xs), p == p2, a.n, p.y));
}
