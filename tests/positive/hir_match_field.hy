// A field read straight off a value `match` (`x ?? y` included) takes the
// match's own layout as the base.
class Cell {
    pub v: int,
    pub tag: string,
}

enum Pick {
    Left,
    Right,
}

fn pick(Pick p, Cell a, Cell b) -> int {
    return (match p {
        Pick::Left => a,
        Pick::Right => b,
    }).v;
}

test("a field of an option coalesce") {
    let some = Option::Some(new Cell(3, "s"));
    let none: Option<Cell> = Option::None;
    assert((some ?? new Cell(0, "d")).v == 3)?;
    assert((none ?? new Cell(9, "d")).tag == "d")?;
}

test("a field of a result coalesce") {
    let ok: Result<Cell, string> = Result::Ok(new Cell(4, "ok"));
    let err: Result<Cell, string> = Result::Err("no");
    assert((ok ?? new Cell(0, "x")).v == 4)?;
    assert((err ?? new Cell(7, "x")).v == 7)?;
}

test("a field of a match") {
    let a = new Cell(1, "a");
    let b = new Cell(2, "b");
    assert(pick(Pick::Left, a, b) == 1)?;
    assert(pick(Pick::Right, a, b) == 2)?;
}
