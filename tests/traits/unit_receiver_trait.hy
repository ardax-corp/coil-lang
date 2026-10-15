// `()` as a method receiver: a trait method on the unit type takes a
// `()` parameter, passed as the empty tuple object and never read. (A
// plain function taking `()` does not compile yet, #1086.)
use tests::assert;

trait Tag<T> {
    fn tag(T x) -> int {}
}

impl Tag for void {
    pub fn tag(() u) -> int {
        return 1;
    }
}

fn tags(int n) -> int {
    let t = 0;
    let i = 0;
    while i < n {
        t = t + ().tag();
        i = i + 1;
    }
    return t;
}

test("unit receiver") {
    assert!(().tag() == 1);
    assert!(tags(3) == 3);
}
