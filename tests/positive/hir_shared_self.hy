// A generic class's shared method bodies lower from HIR when they only
// touch `self`'s closed-type fields and call its other such methods.

class Ring<T> {
    last: Option<T>,
    head: int,
    len: int,
    cap: int,
}

impl Ring<T> {
    pub static fn new() -> Ring<T> {
        return new Ring(Option::None, 0, 0, 4);
    }

    pub fn size() -> int {
        return self.len;
    }

    pub fn slot(int i) -> int {
        return (self.head + i) % self.cap;
    }

    pub fn advance(int n) {
        self.head = self.slot(n);
        self.len = self.len + n;
    }

    pub fn is_full() -> bool {
        return self.size() >= self.cap;
    }
}

test("shared methods read and write closed fields of self") {
    let r = Ring::new();
    assert(r.size() == 0)?;
    r.advance(3);
    assert(r.size() == 3)?;
    assert(r.slot(2) == 1)?;
    assert(!r.is_full())?;
    r.advance(1);
    assert(r.is_full())?;
}
