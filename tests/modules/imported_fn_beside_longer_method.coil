// An imported function and a local inherent method of the same name with
// more parameters: a bare call names the import (#1981).
use tests::assert;
use same_name_method_provider::tag;

class P {
    pub k: int,
}

impl P {
    pub fn tag(int x, int y) -> int {
        return self.k + x + y;
    }
}

test("a bare call names the imported function") {
    let g = tag(1);
    assert!(g == 10);
    let p = new P(100);
    assert!(p.tag(1, 2) == 103);
}
