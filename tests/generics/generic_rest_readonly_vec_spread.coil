// A generic rest pack takes `P` and `readonly P` items together, and a
// `Vec` spread is the whole pack (#1728).
use tests::assert;

trait Reset<R> {
    fn reset(R self) {}

    fn val(readonly R self) -> int {
        return 0;
    }
}

class P {
    pub x: int,
}

impl Reset for P {
    fn reset(P self) {
        self.x = 0;
    }

    fn val(readonly P self) -> int {
        return self.x;
    }
}

fn first<A, B>(A a, B... rest) -> A {
    return a;
}

fn count<B>(B... rest) -> int {
    return rest.len();
}

fn total<A, B: Reset>(A a, B... rest) -> int {
    let n = 0;
    for r in rest {
        n = n + r.val();
    }
    return n;
}

fn all<A, B: Reset>(A a, B... rest) {
    for mut r in rest {
        r.reset();
    }
}

test("a generic rest pack of P and readonly P items") {
    let p = readonly new P(4);
    assert!(first(7, new P(3), p) == 7);
    assert!(first(8, p, new P(3)) == 8);
    assert!(total(0, new P(3), p, new P(5)) == 12);
}

test("a Vec spread is a generic rest pack") {
    let v: Vec<P> = [new P(1), new P(2)];
    assert!(total(0, ...v) == 3);
    all(0, ...v);
    assert!(v[0].x == 0 && v[1].x == 0);
    let w: Vec<int> = [1, 2, 3];
    assert!(count(...w) == 3);
    assert!(first("a", ...w) == "a");
}
