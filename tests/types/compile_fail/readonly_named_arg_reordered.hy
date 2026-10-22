// @expected E0107 — the named `b: p` fills parameter `b`, which `put` mutates through `Reset::reset`, though `p` comes first (#1646).
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

fn put<A, B: Reset>(A a, B b) {
    b.reset();
}

fn main() {
    let p = readonly new P(1);
    put(b: p, a: new P(2));
}
