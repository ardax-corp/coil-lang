// DestProp (#318): slot alias of a class must still read the src fields.

class Cell {
    pub a: int,
    pub b: int,
    pub c: int,
    pub d: int,
}

fn sum_alias(Cell n) -> int {
    let p = n;
    return p.a + p.b + p.c + p.d + p.a + p.b;
}

fn mutate_src_after_alias(Cell n) -> int {
    let p = n;
    let first = p.a;
    n.a = 99;
    return first + p.a;
}

test("field alias sums src fields") {
    assert(sum_alias(new Cell(1, 2, 3, 4)) == 13)?;
}

test("alias sees later field writes on the same object") {
    assert(mutate_src_after_alias(new Cell(1, 2, 3, 4)) == 100)?;
}
