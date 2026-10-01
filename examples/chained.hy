// examples/chained.hy — chained field access.
//
// `Outer` has a record-shaped variant whose `x` field is itself a
// record-shaped enum (`Inner`); `p.x.v` reads through both.
//
// Output: 427

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

enum Inner {
    Inner { v: int },
}

enum Outer {
    Outer { x: Inner, y: int },
}

fn read_x_v(Outer o) -> int {
    return o.x.v;
}

fn read_y(Outer o) -> int {
    return o.y;
}

fn main() {
    let p = Outer::Outer{ x: Inner::Inner{ v: 42 }, y: 7 };
    write_all(stdout(), to_bytes(format("%i", read_x_v(p))));
    write_all(stdout(), to_bytes(format("%i", read_y(p))));
}
