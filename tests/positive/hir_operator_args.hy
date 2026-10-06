// A user type's operator stages its operands through temps, so as a call,
// `format` or host argument, or the right side of `&&`, it runs with no
// operand below it, as the AST stages those arguments first.
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
    assert(pick(7, c == new Cell(42)) == 7)?;
    assert(pick(7, c == new Cell(1)) == 0)?;
    assert(format("%z,%z", Color::Red == Color::Red, Color::Red == Color::Blue) == "true,false")?;
    assert(len(to_bytes(format("%z", c == new Cell(42)))) == 4)?;
    assert(both(new Cell(1), new Cell(1), new Cell(2), new Cell(2)))?;
    assert(!both(new Cell(1), new Cell(1), new Cell(2), new Cell(3)))?;
}
