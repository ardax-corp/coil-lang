// Expected: E0107 — a method whose lambda writes the captured `self` mutates `self` (#1558).
class P {
    pub x: int,
}

impl P {
    pub fn later() {
        let f = fn () use (self) {
            self.x = 5;
        };
        f();
    }
}

fn main() {
    let p = readonly new P(1);
    p.later();
}
