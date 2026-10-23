// examples/aliases.hy — type aliases.
//
// `type X = T;` declares an alias for an existing type. Aliases only exist
// at compile time (no runtime cost); they make long types readable.
//
// @output 347

use io::sync::print;
use string::format;

type Point = (int, int);

fn distance(Point p) -> int {
    let dx = p[0];
    let dy = p[1];
    return dx + dy;
}

fn main() {
    let p: Point = (3, 4);
    let _ = print(format("%i", p[0]));
    let _ = print(format("%i", p[1]));
    let _ = print(format("%i", distance(p)));
}
