// A readonly class of two immediates from another module moves as two
// words on its functions' returns too (#1562).
use tests::assert;
use readonly_pair_geo::{Pt, mid, far};

test("a readonly pair class from another module") {
    let m = mid(new Pt(0.0, 0.0), new Pt(2.0, 2.0));
    assert!(m.x == 1.0);
    assert!(far(4).x == 4.0);
    let v: Vec<Pt> = Vec::new();
    v.push(far(2));
    assert!(v[0] == new Pt(2.0, 0.0));
}
