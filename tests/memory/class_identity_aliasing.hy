// A class instance bound to a local is one heap object: copies of the
// reference, a `Vec` slot, a field of another object and a function parameter
// all see the same fields, and the object outlives a collection while any of
// them holds it.
use tests::assert;
use gc::collect;

class Cell {
    pub n: int,
}

class Box {
    pub inner: Cell,
}

fn bump(Cell c) {
    c.n += 1;
}

fn stash(Vec<Cell> v) -> Cell {
    let c = new Cell(10);
    v.push(c);
    return c;
}

test("two locals name the same object") {
    let a = new Cell(1);
    let b = a;
    b.n = 5;
    assert!(a.n == 5);
}

test("a parameter mutates the caller's object") {
    let a = new Cell(1);
    bump(a);
    bump(a);
    assert!(a.n == 3);
}

test("a Vec slot and a returned reference alias") {
    let v: Vec<Cell> = Vec::new();
    let c = stash(v);
    c.n = 11;
    assert!(v[0].n == 11);
    v[0].n = 12;
    assert!(c.n == 12);
}

test("a field of another object aliases the original") {
    let c = new Cell(2);
    let b = new Box(c);
    b.inner.n = 9;
    assert!(c.n == 9);
}

test("distinct constructions are distinct objects") {
    let a = new Cell(1);
    let b = new Cell(1);
    b.n = 2;
    assert!(a.n == 1 && b.n == 2);
}

test("an object held only by a Vec survives collection") {
    let v: Vec<Cell> = Vec::new();
    let _ = stash(v);
    collect();
    assert!(v[0].n == 10);
}
