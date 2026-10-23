// examples/chained.hy — chained field access.
//
// `Outer` has a record-shaped variant whose `x` field is itself a
// record-shaped enum (`Inner`); `p.x.v` reads through both.
//
// @output 427

use io::sync::print;
use string::format;

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
    let p = Outer::Outer { x: Inner::Inner { v: 42 }, y: 7 };
    let _ = print(format("%i", read_x_v(p)));
    let _ = print(format("%i", read_y(p)));
}
