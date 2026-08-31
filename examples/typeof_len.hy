// examples/typeof_len.hy — static `typeof` / `len` folding and default Show.
//
// Output:
// int
// string
// (int, int)
// 3
// 3
// 2
// 2
// Point
// Point

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

class Point {
    pub x: int,
    pub y: int,
}

fn main() {
    write_all(stdout(), to_bytes(format("%s\n", typeof 42)));
    write_all(stdout(), to_bytes(format("%s\n", typeof "hi")));
    write_all(stdout(), to_bytes(format("%s\n", typeof (1, 2))));
    write_all(stdout(), to_bytes(format("%i\n", len("foo"))));
    write_all(stdout(), to_bytes(format("%i\n", len([1, 2, 3]))));
    write_all(stdout(), to_bytes(format("%i\n", len((1, 2)))));
    write_all(stdout(), to_bytes(format("%i\n", len({ a: 1, b: 2 }))));
    let p = new Point(1, 2);
    write_all(stdout(), to_bytes(format("%s\n", typeof p)));
    write_all(stdout(), to_bytes(format("%v\n", p)));
}
