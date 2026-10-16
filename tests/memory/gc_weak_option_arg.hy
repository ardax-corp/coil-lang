// `gc::weak` and `gc::root` take any `T` (#1414), so an `Option`, a
// `Result` or a two-word value goes to them as a generic argument does:
// boxed to its object, which `upgrade` / `get` / `unroot` hand back.
use tests::assert;
use gc::{get, root, unroot, upgrade, weak};

class Node {
    pub v: int,
}

fn some_node(int v) -> Option<Node> {
    return Option::Some(new Node(v));
}

fn no_node() -> Option<Node> {
    return Option::None;
}

test("weak of an Option<Node> local upgrades to it") {
    let x: Option<Node> = some_node(3);
    let w = weak(x);
    let v = match upgrade(w) {
        Option::Some(Option::Some(n)) => n.v,
        Option::Some(Option::None) => -1,
        Option::None => -2,
    };
    assert!(v == 3);
    assert!(match x {
        Option::Some(n) => n.v == 3,
        Option::None => false,
    });
}

test("weak of a None Option<Node> upgrades to None") {
    let x = no_node();
    let w = weak(x);
    let v = match upgrade(w) {
        Option::Some(Option::Some(n)) => n.v,
        Option::Some(Option::None) => 0,
        Option::None => -2,
    };
    assert!(v == 0);
}

test("weak of a call returning Option<Node>") {
    let w = weak(some_node(5));
    let v = match upgrade(w) {
        Option::Some(Option::Some(n)) => n.v,
        Option::Some(Option::None) => -1,
        Option::None => -2,
    };
    assert!(v == 5);
}

test("root of an Option<Node> local, get and unroot") {
    let x = some_node(7);
    let r = root(x);
    let got = match get(r) {
        Option::Some(Option::Some(n)) => n.v,
        Option::Some(Option::None) => -1,
        Option::None => -2,
    };
    assert!(got == 7);
    let taken = match unroot(r) {
        Option::Some(Option::Some(n)) => n.v,
        Option::Some(Option::None) => -1,
        Option::None => -2,
    };
    assert!(taken == 7);
    assert!(match get(r) {
        Option::Some(_) => false,
        Option::None => true,
    });
}

test("weak of an Option<int>") {
    let x: Option<int> = Option::Some(4);
    let w = weak(x);
    let v = match upgrade(w) {
        Option::Some(Option::Some(n)) => n,
        Option::Some(Option::None) => -1,
        Option::None => -2,
    };
    assert!(v == 4);
}

test("weak of a Result") {
    let x: Result<int, string> = Result::Ok(6);
    let w = weak(x);
    let v = match upgrade(w) {
        Option::Some(Result::Ok(n)) => n,
        Option::Some(Result::Err(_)) => -1,
        Option::None => -2,
    };
    assert!(v == 6);
}

test("weak of a pair tuple") {
    let w = weak((1, 2));
    let v = match upgrade(w) {
        Option::Some(t) => {
            let (a, b) = t;
            a + b
        },
        Option::None => -2,
    };
    assert!(v == 3);
}

test("weak of the Option get returns") {
    let r = root(5);
    let w = weak(get(r));
    let v = match upgrade(w) {
        Option::Some(Option::Some(n)) => n,
        Option::Some(Option::None) => -1,
        Option::None => -2,
    };
    assert!(v == 5);
}
