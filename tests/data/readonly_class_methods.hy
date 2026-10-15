// `readonly` seals a class value against outside writes, but its own methods
// may still mutate fields through `self`; the variable itself can be rebound.
use tests::assert;

class P {
    pub x: int,
}

impl P {
    pub fn bump() {
        self.x = self.x + 1;
    }
}

fn read(P p) -> int {
    return p.x;
}

test("methods mutate a readonly instance") {
    let p = readonly new P(1);
    p.bump();
    p.bump();
    assert!(p.x == 3);
}

test("readonly variable can be rebound") {
    let p = readonly new P(1);
    p = readonly new P(10);
    assert!(p.x == 10);
}
