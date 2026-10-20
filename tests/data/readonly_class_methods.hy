// A method that only reads may be called on a `readonly` instance; one that
// mutates `self` may not (compile_fail/readonly_method_mutates_self.hy). The
// variable itself can be rebound.
use tests::assert;

class P {
    pub x: int,
}

impl P {
    pub fn get() -> int {
        return self.x;
    }

    pub fn bump() {
        self.x = self.x + 1;
    }

    pub fn plus(int n) -> P {
        return new P(self.get() + n);
    }
}

test("reading methods on a readonly instance") {
    let p = readonly new P(1);
    assert!(p.get() == 1);
    assert!(p.plus(2).get() == 3);
}

test("the mutable name still mutates") {
    let m = new P(1);
    let p = readonly m;
    m.bump();
    m.bump();
    assert!(p.get() == 3);
}

test("readonly variable can be rebound") {
    let p = readonly new P(1);
    p = readonly new P(10);
    assert!(p.x == 10);
}
