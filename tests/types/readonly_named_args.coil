// A `readonly` value passed by name goes to the parameter it fills: one
// that is only read accepts it, wherever it is written (#1646).
use tests::assert;

trait Reset<R> {
    fn reset(R self) {}
}

class P {
    pub x: int,
}

impl Reset for P {
    fn reset(P self) {
        self.x = 0;
    }
}

class H {
    pub n: int,
}

impl H {
    pub fn put<A, B: Reset>(A a, B b) {
        b.reset();
    }
}

test("a readonly named argument to a parameter that is only read") {
    let p = readonly new P(1);
    let q = new P(2);
    let h = new H(0);
    h.put(b: q, a: p);
    assert!(q.x == 0);
    assert!(p.x == 1);
}
