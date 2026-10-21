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
// Output: int\nstring\n(int, int)\n3\n3\n2\n2\nPoint\nPoint\n

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

class Point {
    pub x: int,
    pub y: int,
}

fn main() {
    let _ = write_all(stdout(), to_bytes(format("%s\n", typeof 42)));
    let _ = write_all(stdout(), to_bytes(format("%s\n", typeof "hi")));
    let _ = write_all(stdout(), to_bytes(format("%s\n", typeof (1, 2))));
    let _ = write_all(stdout(), to_bytes(format("%i\n", "foo".len())));
    let _ = write_all(stdout(), to_bytes(format("%i\n", [1, 2, 3].len())));
    let _ = write_all(stdout(), to_bytes(format("%i\n", (1, 2).len())));
    let _ = write_all(stdout(), to_bytes(format("%i\n", ({ a: 1, b: 2 }).len())));
    let p = new Point(1, 2);
    let _ = write_all(stdout(), to_bytes(format("%s\n", typeof p)));
    let _ = write_all(stdout(), to_bytes(format("%v\n", p)));
}
