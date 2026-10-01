// examples/aliases.hy — type aliases.
//
// `type X = T;` declares an alias for an existing type. Aliases only exist
// at compile time (no runtime cost); they make long types readable.
//
// Output: 347

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
