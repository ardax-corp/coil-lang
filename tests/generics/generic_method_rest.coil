// A method with a rest parameter, generic or not, takes its pack as a
// free function does (#1729).
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

class H {
    pub n: int,
}

impl H {
    pub fn all<B: Reset>(int k, B... rest) -> int {
        let n = k;
        for r in rest {
            n = n + r.val();
            r.reset();
        }
        return n;
    }

    pub fn count<B>(B... rest) -> int {
        return self.n + rest.len();
    }

    pub fn sum(int... xs) -> int {
        let s = self.n;
        for x in xs {
            s = s + x;
        }
        return s;
    }
}

class G<T> {
    pub v: T,
}

impl G<T> {
    pub fn sum(int... xs) -> int {
        let s = 0;
        for x in xs {
            s = s + x;
        }
        return s;
    }

    pub fn count<B>(B... xs) -> int {
        return xs.len();
    }
}

test("a generic method with a rest parameter") {
    let h = new H(10);
    let p = new P(2);
    assert!(h.all(1, p, new P(3)) == 6);
    assert!(p.x == 0);
    assert!(h.count("a", "b") == 12);
    assert!(h.count(1, 2, 3) == 13);
    let q = readonly new P(5);
    assert!(h.count(new P(1), q) == 12);
}

test("a generic class's method with a rest parameter") {
    let g = new G(1.5);
    assert!(g.sum(1, 2) == 3);
    assert!(g.count("a", "b", "c") == 3);
}

test("a method with a rest parameter") {
    let h = new H(10);
    assert!(h.sum(1, 2, 3) == 16);
    assert!(h.sum() == 10);
    assert!(h.sum(...[4, 5]) == 19);
    let v: Vec<int> = [6, 7];
    assert!(h.sum(...v) == 23);
}

test("a Vec spread into a generic method's rest") {
    let h = new H(0);
    let v: Vec<P> = [new P(1), new P(2)];
    assert!(h.all(0, ...v) == 3);
    assert!(v[0].x == 0 && v[1].x == 0);
}
