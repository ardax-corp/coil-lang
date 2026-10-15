// `gc::root` keeps a value alive with no other reference; `unroot` hands it
// back once and clears the pin; `weak` never keeps a heap value alive, and a
// weak to an immediate always upgrades.
use tests::assert;
use gc::{collect, get, root, unroot, weak, upgrade, Root, Weak};

class Cell {
    pub n: int,
}

fn pinned_cell(int n) -> Root<Cell> {
    return root(new Cell(n));
}

fn weak_only(int n) -> Weak<Cell> {
    let c = new Cell(n);
    return weak(c);
}

fn pinned_and_weak(int n) -> (Root<Cell>, Weak<Cell>) {
    let c = new Cell(n);
    return (root(c), weak(c));
}

fn alive(Weak<Cell> w) -> bool {
    return match upgrade(w) {
        Option::Some(_) => true,
        Option::None => false,
    };
}

fn release(Root<Cell> r) -> int {
    return match unroot(r) {
        Option::Some(c) => c.n,
        Option::None => -1,
    };
}

test("a root alone keeps its value across collections") {
    let r = pinned_cell(7);
    collect();
    collect();
    let n = match get(r) {
        Option::Some(c) => c.n,
        Option::None => -1,
    };
    assert!(n == 7);
}

test("a weak alone does not keep its value") {
    let w = weak_only(3);
    collect();
    assert!(!alive(w));
}

test("a weak upgrades while a root holds the value, and not after unroot") {
    let pair = pinned_and_weak(5);
    let r = pair[0];
    let w = pair[1];
    collect();
    assert!(alive(w));
    assert!(release(r) == 5);
    collect();
    assert!(!alive(w));
}

test("unroot hands the value back once") {
    let r = pinned_cell(9);
    assert!(release(r) == 9);
    assert!(release(r) == -1);
    let after = match get(r) {
        Option::Some(_) => 1,
        Option::None => 0,
    };
    assert!(after == 0);
}

test("a weak to an immediate always upgrades") {
    let w = weak(42);
    collect();
    let v = match upgrade(w) {
        Option::Some(n) => n,
        Option::None => -1,
    };
    assert!(v == 42);
    let b = weak(true);
    collect();
    assert!(upgrade(b) == Option::Some(true));
}

test("weak entries in a registry do not extend lifetimes") {
    let reg: Vec<Weak<Cell>> = Vec::new();
    let keep = new Cell(1);
    reg.push(weak(keep));
    reg.push(weak_only(2));
    reg.push(weak_only(3));
    collect();
    let live = 0;
    for w in reg {
        if alive(w) {
            live += 1;
        }
    }
    assert!(live == 1);
    assert!(keep.n == 1);
}
