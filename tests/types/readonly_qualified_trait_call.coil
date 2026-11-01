// `Trait::m(p)` on a concrete receiver checks the impl's body, as
// `p.m()` does: an impl that only reads accepts a `readonly` value even
// when its trait does not declare `self` `readonly` (#1647).
use tests::assert;

trait Peek<S> {
    fn peek(S self) -> int {}
}

class P {
    pub x: int,
}

impl Peek for P {
    fn peek(P self) -> int {
        return self.x;
    }
}

test("a qualified trait call on a readonly receiver that only reads") {
    let p = readonly new P(4);
    assert!(p.peek() == 4);
    assert!(Peek::peek(p) == 4);
}
