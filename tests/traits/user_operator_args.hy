// An operator over a user type works as a call argument, a `format` or
// host-call argument and on the right side of `&&`.
use tests::assert;
use string::{format, to_bytes};

#[derive(Eq)]
class Cell {
    pub v: int,
}

#[derive(Eq)]
enum Color {
    Red,
    Blue,
}

fn both(Cell a, Cell b, Cell c, Cell d) -> bool {
    return (a == b) && (c == d);
}

fn pick(int a, bool b) -> int {
    if b {
        return a;
    }
    return 0;
}

test("operators as arguments") {
    let c = new Cell(42);
    assert!(pick(7, c == new Cell(42)) == 7);
    assert!(pick(7, c == new Cell(1)) == 0);
    assert!(format("%z,%z", Color::Red == Color::Red, Color::Red == Color::Blue) == "true,false");
    assert!(len(to_bytes(format("%z", c == new Cell(42)))) == 4);
    assert!(both(new Cell(1), new Cell(1), new Cell(2), new Cell(2)));
    assert!(!both(new Cell(1), new Cell(1), new Cell(2), new Cell(3)));
}
