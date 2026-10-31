// A free function and an inherent method of the same name with more
// parameters: a bare call names the free function, never a partial
// application of the method (#1981).
use tests::assert;

fn tag(int x) -> int {
    return x * 10;
}

class P {
    pub k: int,
}

impl P {
    pub fn tag(int x, int y) -> int {
        return self.k + x + y;
    }
}

test("a bare call names the free function") {
    let g = tag(1);
    assert!(g == 10);
    assert!(tag(4) == 40);
}

test("the method still takes its own arity") {
    let p = new P(100);
    assert!(p.tag(1, 2) == 103);
    let f = p.tag(5);
    assert!(f(6) == 111);
}
