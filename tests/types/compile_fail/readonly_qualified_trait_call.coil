// @expected E0107 — `Reset::reset(p)` runs `P`'s impl, which writes `self.x`, on the `readonly` `p` (#1647).
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

fn main() {
    let p = readonly new P(1);
    Reset::reset(p);
}
