// Inherent methods call module functions declared before or after the
// `impl`, including nullary helpers, helpers with an inferred return type,
// and generic helpers.
use tests::assert;

fn before(int n) -> int {
    return n + 1;
}

class Foo {
    pub v: int,
}

impl Foo {
    pub fn early() -> int {
        return before(self.v);
    }

    pub fn late() -> int {
        return after(self.v);
    }

    pub fn mixed() -> int {
        return one() + inferred(self.v);
    }

    pub fn generic() -> int {
        return id(self.v);
    }
}

fn after(int n) -> int {
    return n + 1;
}

fn one() {
    return 1;
}

fn inferred(int n) {
    return n;
}

fn id<T>(T x) -> T {
    return x;
}

test("helpers declared before and after the impl") {
    let f = new Foo(1);
    assert!(f.early() == 2);
    assert!(f.late() == 2);
}

test("nullary and inferred-return helpers") {
    let f = new Foo(1);
    assert!((typeof f.mixed()) == "int");
    assert!(f.mixed() == 2);
}

test("a generic helper") {
    let f = new Foo(5);
    assert!(f.generic() == 5);
}
