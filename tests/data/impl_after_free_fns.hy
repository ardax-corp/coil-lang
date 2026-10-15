// An `impl` may come after the free functions and tests that call its
// methods.
use tests::assert;

class Counter {
    n: int,
}

fn twice(int n) -> int {
    return n * 2;
}

fn total(Counter c) -> int {
    return twice(c.get()) + 1;
}

test("free functions after a trailing impl lower") {
    let c = new Counter(4);
    c.bump();
    assert!(total(c) == 11);
    assert!(twice(3) == 6);
}

impl Counter {
    pub fn get() -> int {
        return self.n;
    }

    pub fn bump() {
        self.n = self.n + 1;
    }
}
