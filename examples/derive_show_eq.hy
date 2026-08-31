// examples/derive_show_eq.hy — `#[derive(...)]` for Show / Eq / Ord.
//
// Output: Color::Red,true,false,true,Point::Point { x: 5, y: 12 },true,false,Cell { value: 42 },true,false

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
#[derive(Show, Eq, Ord)]
enum Color {
    Red,
    Blue,
}

#[derive(Show, Eq)]
enum Point {
    Origin,
    Point { x: int, y: int },
}

#[derive(Show, Eq)]
class Cell {
    pub value: int,
}

fn main() {
    write_all(stdout(), to_bytes(format("%v,", Color::Red)));
    write_all(stdout(), to_bytes(format("%z,", Color::Red == Color::Red)));
    write_all(stdout(), to_bytes(format("%z,", Color::Red == Color::Blue)));
    write_all(stdout(), to_bytes(format("%z,", Color::Red < Color::Blue)));

    let p = Point::Point { x: 5, y: 12 };
    write_all(stdout(), to_bytes(format("%v,", p)));
    write_all(stdout(), to_bytes(format("%z,", p == Point::Point { x: 5, y: 12 })));
    write_all(stdout(), to_bytes(format("%z,", p == Point::Origin)));

    let c = new Cell(42);
    write_all(stdout(), to_bytes(format("%v,", c)));
    write_all(stdout(), to_bytes(format("%z,", c == new Cell(42))));
    write_all(stdout(), to_bytes(format("%z", c == new Cell(7))));
}
