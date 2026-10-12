// A field read kept for a later return must happen before the write that
// replaces the field, in every tier.
class Cell {
    pub v: int,
    pub s: string,
    pub o: Option<string>,
}

fn take_int(Cell c) -> int {
    let old = c.v;
    c.v = 0;
    return old;
}

fn take_str(Cell c) -> string {
    let old = c.s;
    c.s = "gone";
    return old;
}

fn take_opt(Cell c) -> Option<string> {
    let old = c.o;
    c.o = Option::None;
    return old;
}

fn swap_sum(Vec<int> xs) -> int {
    let a = xs[0];
    xs[0] = xs[1];
    xs[1] = a;
    return a;
}

test("a field read before its write") {
    let c = new Cell(5, "s", Option::Some("o"));
    assert(take_int(c) == 5)?;
    assert(c.v == 0)?;
    assert(take_str(c) == "s")?;
    assert(c.s == "gone")?;
    let o = take_opt(c);
    assert((o ?? "none") == "o")?;
    assert((c.o ?? "none") == "none")?;
}

test("an element read before its write") {
    let xs: Vec<int> = Vec::new();
    xs.push(1);
    xs.push(2);
    assert(swap_sum(xs) == 1)?;
    assert(xs[0] == 2)?;
    assert(xs[1] == 1)?;
}
