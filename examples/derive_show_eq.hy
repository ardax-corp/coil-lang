// examples/derive_show_eq.hy — `#[derive(...)]` for Show / Eq / Ord.
//
// @output Color::Red,true,false,true,Point::Point { x: 5, y: 12 },true,false,Cell { value: 42 },true,false

use io::sync::print;
use string::format;

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
    let _ = print(format("%v,", Color::Red));
    let _ = print(format("%z,", Color::Red == Color::Red));
    let _ = print(format("%z,", Color::Red == Color::Blue));
    let _ = print(format("%z,", Color::Red < Color::Blue));

    let p = Point::Point { x: 5, y: 12 };
    let _ = print(format("%v,", p));
    let _ = print(format("%z,", p == Point::Point { x: 5, y: 12 }));
    let _ = print(format("%z,", p == Point::Origin));

    let c = new Cell(42);
    let _ = print(format("%v,", c));
    let _ = print(format("%z,", c == new Cell(42)));
    let _ = print(format("%z", c == new Cell(7)));
}
