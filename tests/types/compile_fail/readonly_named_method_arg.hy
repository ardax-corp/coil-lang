// @expected E0107 — `h.put(a: q, b: p)` passes the `readonly` `p` to parameter `b`, which `H::put` mutates (#1646).
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

fn main() {
    let p = readonly new P(1);
    let h = new H(0);
    h.put(a: new P(2), b: p);
}
