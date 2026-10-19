// Two-word values (pairs) held boxed in record and class fields, and a
// record `let` pattern, on direct MIR.
use tests::assert;

class Holder {
    pub p: (int, int),
    pub r: Range<int>,
}

fn pair(int i) -> (int, int) {
    return (i, i * 2);
}

test("record fields hold pairs boxed") {
    let rec = { p: pair(3), n: 4 };
    rec.p = pair(5);
    let { p, n } = rec;
    assert!(p[0] + p[1] + n == 19);
}

test("class fields hold pairs boxed") {
    let h = new Holder(pair(2), 0..3);
    let acc = h.p.len();
    for x in h.r {
        acc = acc + x;
    }
    h.r = 1..4;
    for x in h.r {
        acc = acc + x;
    }
    assert!(acc == 11);
    assert!(h.p[1] == 4);
}
