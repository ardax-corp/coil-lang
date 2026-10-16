// #1204: an inherent `impl` on an enum: a `static fn` is called as
// `E::make()`, and a method can be called on a constructor expression.
use tests::assert;

enum E {
    A,
    B(int),
}

impl E {
    pub fn tag() -> int {
        return match self {
            E::A => 1,
            E::B(n) => n,
        };
    }

    pub static fn make() -> int {
        return 2;
    }

    pub static fn build(int n) -> E {
        return E::B(n);
    }
}

test("a static fn on an enum is called through the enum") {
    assert!(E::make() == 2);
    let e = E::build(7);
    assert!(e.tag() == 7);
}

test("a method is called on a constructor expression") {
    assert!(E::A.tag() == 1);
    assert!(E::B(5).tag() == 5);
}

test("a method on a local still works") {
    let e = E::A;
    assert!(e.tag() == 1);
}
