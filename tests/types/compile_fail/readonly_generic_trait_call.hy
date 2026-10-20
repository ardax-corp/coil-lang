// Expected: E0107 — `clear` mutates `v` through `Reset::reset`, which its trait does not declare `readonly A self` (#1558).
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

fn clear<T: Reset>(T v) {
    v.reset();
}

fn main() {
    let p = readonly new P(1);
    clear(p);
}
