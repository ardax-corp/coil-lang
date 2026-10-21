// Expected: E0107 — `...t` passes the `readonly` `p` as parameter `b`, which `put` mutates through `Reset::reset` (#1646).
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
    let t = (new P(2), p);
    put(...t);
}
