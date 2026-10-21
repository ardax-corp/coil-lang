// A `readonly` value packed into a rest parameter is checked against that
// parameter, wherever it is packed: one that is only read accepts it, while
// the fixed parameter is still mutated (#1681).
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

fn first<A: Reset, B>(A a, B... rest) -> int {
    a.reset();
    return rest.len();
}

test("a readonly value packed into a rest parameter that is only read") {
    let p = readonly new P(1);
    let q = new P(2);
    assert!(first(q, p, p) == 2);
    assert!(q.x == 0);
    assert!(p.x == 1);
}
