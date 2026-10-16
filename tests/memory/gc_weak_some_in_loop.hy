// An object wrapped in `Option::Some` inside a loop must not stay rooted by
// the register the `Some` was cast from: once the `Option` is rebound and no
// strong reference is left, a `collect` clears a weak to the object, at every
// optimization level and whether or not the helpers are inlined (#1411).
use tests::assert;
use gc::{collect, upgrade, weak, Weak};

class Node {
    pub v: int,
    pub next: Option<Node>,
}

fn gone<T>(Weak<T> w) -> bool {
    return match upgrade(w) {
        Option::Some(_) => false,
        Option::None => true,
    };
}

// A weak to the wrapped object, without a temporary that names it.
fn weak_of(Option<Node> o) -> Weak<Node> {
    return match o {
        Option::Some(n) => weak(n),
        Option::None => weak(new Node(-1, Option::None)),
    };
}

// The shape of `examples/perf/gc_int_vec`: a list that grows, then is dropped.
fn churn(Option<Node> junk, int round) -> Option<Node> {
    if round % 4 == 0 {
        return Option::None;
    }
    return Option::Some(new Node(round, junk));
}

test("a Some of a fresh object is dropped when the Option is rebound") {
    let junk: Option<Node> = Option::None;
    let i = 0;
    let ok = true;
    while i < 3 {
        junk = Option::Some(new Node(i, Option::None));
        let w = weak_of(junk);
        junk = Option::None;
        collect();
        ok = ok && gone(w);
        i = i + 1;
    }
    assert!(ok);
}

test("the previous iteration's object is gone before the next allocation") {
    let junk: Option<Node> = Option::None;
    let w = weak_of(junk);
    let i = 0;
    let ok = true;
    while i < 4 {
        collect();
        ok = ok && gone(w);
        junk = Option::Some(new Node(i, Option::None));
        w = weak_of(junk);
        junk = Option::None;
        i = i + 1;
    }
    assert!(ok);
}

test("the last object a churn loop wrapped is dropped after the loop") {
    let junk: Option<Node> = Option::None;
    let round = 1;
    while round < 4 {
        junk = churn(junk, round);
        round = round + 1;
    }
    let w = weak_of(junk);
    junk = Option::None;
    collect();
    assert!(gone(w));
}

test("the object stays alive while the Option still holds it") {
    let junk: Option<Node> = Option::None;
    let i = 0;
    let ok = true;
    while i < 3 {
        junk = Option::Some(new Node(i, Option::None));
        let w = weak_of(junk);
        collect();
        ok = ok && !gone(w);
        i = i + 1;
    }
    assert!(ok);
    let n = match junk {
        Option::Some(n) => n.v,
        Option::None => -1,
    };
    assert!(n == 2);
}
