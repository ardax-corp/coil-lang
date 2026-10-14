// Shapes direct MIR lowers from HIR: a stack array store at an index the
// checker cannot prove in range (in range it writes the slot, out of range
// it panics on a boxed copy), a stack array of niche `Option` elements,
// `!` of an int, and a `while false` loop that never runs.
use tests::assert;

class Node {
    pub v: int,
}

fn stores(int k) -> int {
    let xs = [10, 20, 30];
    xs[k] = 7;
    let m = 2;
    xs[k % m] = 3;
    return xs[0] * 100 + xs[1] * 10 + xs[2];
}

fn holes(int k) -> int {
    let hs = [Option::Some(new Node(5)), Option::None, Option::Some(new Node(7))];
    let total = 0;
    for h in hs {
        total = total + match h {
            Option::Some(n) => n.v,
            Option::None => 0,
        };
    }
    let picked = match hs[k] {
        Option::Some(n) => n.v,
        Option::None => -1,
    };
    return total * 10 + picked;
}

fn nots(int x) -> bool {
    let a = !0;
    let b = !x;
    return a && !b;
}

fn skipped(int n) -> int {
    let out = n;
    while false {
        out = out + 100;
    }
    return out;
}

test("unproven stack array stores") {
    assert!(stores(1) == 1060);
    assert!(stores(2) == 507);
}

test("stack array of niche options") {
    assert!(holes(0) == 125);
    assert!(holes(1) == 119);
}

test("logical not of an int") {
    assert!(nots(5));
    assert!(!nots(0));
}

test("while false never runs") {
    assert!(skipped(4) == 4);
}
