// @expected E0107 — a method that mutates `self` cannot be called on a `readonly` instance (#1558).
class P {
    pub x: int,
}

impl P {
    pub fn bump() {
        self.x = self.x + 1;
    }
}

fn main() {
    let p = readonly new P(1);
    p.bump();
}
