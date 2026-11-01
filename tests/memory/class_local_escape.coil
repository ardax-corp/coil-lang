// A class local kept in frame slots is boxed into its object where it
// escapes; field reads and writes after that go through the object, in
// direct MIR as in HIR emission. Classes are copy-on-write: a write after
// the object was stored lands on a fresh copy, so the stored value keeps
// what it had at the store.
use tests::assert;

class Cell {
    pub v: int,
    pub next: Option<Cell>,
}

fn chain(int n) -> int {
    let head: Option<Cell> = Option::None;
    let i = 0;
    while i < n {
        let c = new Cell(i, Option::None);
        c.v = c.v * 10;
        c.next = head;
        head = Option::Some(c);
        c.v = c.v + 1;
        i = i + 1;
    }
    let sum = 0;
    let go = true;
    while go {
        match head {
            Option::Some(c) => {
                sum = sum + c.v;
                head = c.next;
            },
            Option::None => {
                go = false;
            },
        }
    }
    return sum;
}

fn keep(Cell c) -> Cell {
    return c;
}

fn after_escape(int a) -> int {
    let c = new Cell(a, Option::None);
    c.v = c.v + 1;
    let k = keep(c);
    c.v = c.v * 2;
    return k.v + c.v;
}

test("frame-slot class locals boxed at their escape") {
    assert!(chain(4) == 60);
    assert!(after_escape(3) == 12);
}
