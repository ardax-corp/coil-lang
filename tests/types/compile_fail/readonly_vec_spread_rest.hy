// Expected: E0107 — `...v` spreads `readonly` values into `rest`, which `all` mutates through `Reset::reset` (#1681).
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

fn all<A, B: Reset>(A a, B... rest) {
    for r in rest {
        r.reset();
    }
}

fn main() {
    let p = readonly new P(1);
    let v: Vec<readonly P> = Vec::new();
    v.push(p);
    all(0, ...v);
}
