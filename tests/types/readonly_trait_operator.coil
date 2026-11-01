// A trait operator on a `readonly` value runs its instance when the impl
// only reads its operands (#1645).
use tests::assert;

class V {
    pub x: int,
}

impl Add for V {
    fn add(V self, V o) -> V {
        return new V(self.x + o.x);
    }
}

fn sum(readonly V a, readonly V b) -> int {
    return (a + b).x;
}

test("a readonly operand of a reading operator") {
    let ro = readonly new V(1);
    assert!((ro + new V(2)).x == 3);
    assert!((new V(4) + ro).x == 5);
    assert!(sum(ro, ro) == 2);
}
