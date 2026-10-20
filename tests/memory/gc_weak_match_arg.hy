// A binding a `match` or `if let` arm passes to a call must not stay rooted by
// the register the call's argument was set up in: once the value it came from
// is rebound and no strong reference is left, a `collect` clears a weak to the
// object, at every optimization level and whether or not the helpers are
// inlined (#1625).
use tests::assert;
use gc::{collect, upgrade, weak, Weak};

class Node {
    pub v: int,
    pub next: Option<Node>,
}

class Maker {
    pub k: int,
}

impl Maker {
    pub fn weak_of(Node n) -> Weak<Node> {
        return weak(n);
    }
}

fn gone<T>(Weak<T> w) -> bool {
    return match upgrade(w) {
        Option::Some(_) => false,
        Option::None => true,
    };
}

fn weak_to(Node n) -> Weak<Node> {
    return weak(n);
}

// Three arguments, so a call gathers them into its own window.
fn pick(int k, Node a, Node b) -> Weak<Node> {
    if k > 0 {
        return weak(b);
    }
    return weak(a);
}

test("a match arm's binding passed to a host call is dropped") {
    let junk: Option<Node> = Option::None;
    let i = 0;
    let ok = true;
    while i < 3 {
        junk = Option::Some(new Node(i, Option::None));
        let w = match junk {
            Option::Some(n) => weak(n),
            Option::None => weak(new Node(-1, Option::None)),
        };
        junk = Option::None;
        collect();
        ok = ok && gone(w);
        i = i + 1;
    }
    assert!(ok);
}

test("a match arm's binding passed to a function is dropped") {
    let junk: Option<Node> = Option::None;
    let i = 0;
    let ok = true;
    while i < 3 {
        junk = Option::Some(new Node(i, Option::None));
        let w = match junk {
            Option::Some(n) => weak_to(n),
            Option::None => weak_to(new Node(-1, Option::None)),
        };
        junk = Option::None;
        collect();
        ok = ok && gone(w);
        i = i + 1;
    }
    assert!(ok);
}

test("a match arm's binding passed to a method is dropped") {
    let m = new Maker(1);
    let junk: Option<Node> = Option::None;
    let i = 0;
    let ok = true;
    while i < 3 {
        junk = Option::Some(new Node(i, Option::None));
        let w = match junk {
            Option::Some(n) => m.weak_of(n),
            Option::None => m.weak_of(new Node(-1, Option::None)),
        };
        junk = Option::None;
        collect();
        ok = ok && gone(w);
        i = i + 1;
    }
    assert!(ok && m.k == 1);
}

test("a binding gathered with other arguments is dropped") {
    let other = new Node(7, Option::None);
    let junk: Option<Node> = Option::None;
    let i = 0;
    let ok = true;
    while i < 3 {
        junk = Option::Some(new Node(i, Option::None));
        let w = match junk {
            Option::Some(n) => pick(i + 1, other, n),
            Option::None => weak(other),
        };
        junk = Option::None;
        collect();
        ok = ok && gone(w);
        i = i + 1;
    }
    assert!(ok && !gone(weak(other)));
}

test("an if let binding passed to a call is dropped") {
    let junk: Option<Node> = Option::None;
    let i = 0;
    let ok = true;
    while i < 3 {
        junk = Option::Some(new Node(i, Option::None));
        let w = weak(new Node(-1, Option::None));
        if let Option::Some(n) = junk {
            w = weak(n);
        }
        junk = Option::None;
        collect();
        ok = ok && gone(w);
        i = i + 1;
    }
    assert!(ok);
}

test("a match statement's binding passed to a call is dropped") {
    let junk: Option<Node> = Option::None;
    let i = 0;
    let ok = true;
    while i < 3 {
        junk = Option::Some(new Node(i, Option::None));
        let w = weak(new Node(-1, Option::None));
        match junk {
            Option::Some(n) => {
                w = weak(n);
            },
            Option::None => {},
        }
        junk = Option::None;
        collect();
        ok = ok && gone(w);
        i = i + 1;
    }
    assert!(ok);
}

test("the object stays alive while the Option still holds it") {
    let junk: Option<Node> = Option::None;
    let i = 0;
    let ok = true;
    while i < 3 {
        junk = Option::Some(new Node(i, Option::None));
        let w = match junk {
            Option::Some(n) => weak(n),
            Option::None => weak(new Node(-1, Option::None)),
        };
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
