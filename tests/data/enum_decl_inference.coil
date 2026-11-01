// Enum declarations: variants keep declaration order, payloads may refer to
// the enum itself or to an enum declared later, two enums may share a case
// name, and a user `Status::Ok` coexists with `Result::Ok`.
use tests::assert;

#[derive(Eq, Ord)]
enum E {
    Z,
    A,
    M,
    B,
}

enum Tree {
    Leaf,
    Node(int, Tree, Tree),
}

enum First {
    Foo,
}

enum Second {
    Foo,
}

enum Status {
    Ok = 200,
    NotFound = 404,
}

fn sum(Tree t) -> int {
    return match t {
        Tree::Leaf => 0,
        Tree::Node(v, l, r) => v + sum(l) + sum(r),
    };
}

test("variants are ordered by declaration, not by name") {
    assert!(E::Z < E::A);
    assert!(E::A < E::M);
    assert!(E::M < E::B);
}

test("a recursive payload") {
    let t = Tree::Node(1, Tree::Node(2, Tree::Leaf, Tree::Leaf), Tree::Leaf);
    assert!((typeof t) == "Tree");
    assert!(sum(t) == 3);
}

test("two enums may declare the same case") {
    let a = First::Foo;
    let b = Second::Foo;
    assert!((typeof a) == "First");
    assert!((typeof b) == "Second");
}

test("Status::Ok and Result::Ok coexist") {
    let s = Status::Ok;
    let r = Result::Ok(1);
    let n: int = s;
    assert!(n == 200);
    let ok = match r {
        Result::Ok(v) => v,
        Result::Err(_) => 0,
    };
    assert!(ok == 1);
}
