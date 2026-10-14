// A bounded generic instance's method reads its context dictionaries out
// of its own dictionary at entry; on direct MIR each is read once there
// and its bound calls go through it.
use tests::assert;

trait Size<T> {
    fn size(T self) -> int {}
}

impl Size for int {
    fn size(int self) -> int {
        return self;
    }
}

impl Size for string {
    fn size(string self) -> int {
        return len(self);
    }
}

trait Total<T> {
    fn total(T self) -> int {}
}

class Duo<T> {
    pub a: T,
    pub b: T,
}

impl Total for Duo<T: Size> {
    pub fn total(Duo<T> d) -> int {
        let n = 0;
        let i = 0;
        while i < 3 {
            // Allocates: the context dictionary stays live across it.
            let label = "#" + i.show();
            n += d.a.size() + d.b.size() + len(label);
            i += 1;
        }
        return n;
    }
}

fn totals<T: Size>(T x, T y) -> int {
    return new Duo(x, y).total();
}

test("a context dictionary through a shared instance") {
    let t = totals;
    assert!(t(3, 4) == 3 * 9);
    assert!(t("ab", "cde") == 3 * 7);
}

test("a context dictionary through a mono instance") {
    assert!(totals(1, 2) == 3 * 5);
}
