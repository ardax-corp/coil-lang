// examples/typeof_len.hy — static `typeof` / `len` folding and default Show.
//
// int
// string
// (int, int)
// 3
// 3
// 2
// 2
// Point
// Point
//
// @output int\nstring\n(int, int)\n3\n3\n2\n2\nPoint\nPoint\n

use io::sync::println;
use string::format;

class Point {
    pub x: int,
    pub y: int,
}

fn main() {
    let _ = println(format("%s", typeof 42));
    let _ = println(format("%s", typeof "hi"));
    let _ = println(format("%s", typeof (1, 2)));
    let _ = println(format("%i", "foo".len()));
    let _ = println(format("%i", [1, 2, 3].len()));
    let _ = println(format("%i", (1, 2).len()));
    let _ = println(format("%i", ({ a: 1, b: 2 }).len()));
    let p = new Point(1, 2);
    let _ = println(format("%s", typeof p));
    let _ = println(format("%v", p));
}
