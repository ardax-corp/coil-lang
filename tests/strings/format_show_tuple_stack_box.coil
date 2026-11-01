// `%v` of a tuple or record in a body that also boxes a stack array or a
// frame-slot class local (#1452): the box and the shown fields both lower.
use tests::assert;
use string::format;

class Cell {
    pub a: int,
    pub b: int,
}

fn first([int; 3] xs) -> int {
    return xs[0];
}

fn total(Cell c) -> int {
    return c.a + c.b;
}

test("stack array boxed before a tuple show") {
    let xs = [1, 2, 3];
    let n = first(xs);
    let t = (7, "x");
    assert!(format("%v", t) == "(7, x)");
    assert!(n == 1);
}

test("stack array boxed in the format statement") {
    let xs = [4, 5, 6];
    let s = format("%i %v", first(xs), (true, 2));
    assert!(s == "4 (true, 2)");
}

test("class local boxed next to a record show") {
    let c = new Cell(1, 2);
    c.a = 3;
    let r = total(c);
    assert!(format("%v", { a: r, b: c.b }) == "{ a: 5, b: 2 }");
}
