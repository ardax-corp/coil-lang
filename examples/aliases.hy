// examples/aliases.hy — Phase 28 type aliases.
//
// `type X = T;` declares an alias that is substituted at
// typecheck time. Aliases are zero-cost (no runtime effect)
// and purely source-level: they make types more readable in
// bigger programs.
//
// Runtime output:
//   3
//   4
//   7

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

type Point = (int, int);

fn distance(Point p) -> int {
    let dx = p[0];
    let dy = p[1];
    return dx + dy;
}

fn main() {
    let p: Point = (3, 4);
    write_all(stdout(), to_bytes(format("%i", p[0])));
    write_all(stdout(), to_bytes(format("%i", p[1])));
    write_all(stdout(), to_bytes(format("%i", distance(p))));
}
