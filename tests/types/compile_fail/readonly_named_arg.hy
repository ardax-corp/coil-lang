// Expected: E0107 — `mutate(v: p)` passes the `readonly` `p` to parameter `v`, which `Reset::reset` mutates (#1646).
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

fn mutate<T: Reset>(T v) {
    v.reset();
}

fn main() {
    let p = readonly new P(1);
    mutate(v: p);
}
