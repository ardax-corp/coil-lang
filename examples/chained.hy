// examples/chained.hy — chained field access (Phase 19).
//
// `Outer` has a record-shaped variant whose `x` field is itself
// a record-shaped enum (`Inner`). Reading `p.x.v` chains two
// field accesses: the inner `p.x` reads Outer's `x` slot
// (which holds an `Inner` value), and the OUTER `.v` reads
// Inner's `v` slot.
//
// Phase 19 fixes the codegen for this case — pre-19, the
// OUTER access's receiver was `Access(p, "x")` and the codegen
// resolved the receiver's enum as `Outer`, so the OUTER
// `LoadField` was indexed against `Outer`'s record (where
// `v` doesn't exist) and silently read slot 0 (the `x`
// value, which is an enum, not an int).
//
// Output: "42" + "7" = "427".
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
