// A trait-annotated `let` or `return` of a call that inlines packs the
// call's result (#1757): the inlined body's result is the packed value, so
// it must still take the pack, for a plain and a readonly class alike.
use tests::assert;

trait Sum<A> {
    fn total(A self) -> int {}
}

class P {
    pub a: int,
    pub b: int,
}

readonly class R {
    pub a: int,
    pub b: int,
}

impl Sum for P {
    fn total(P self) -> int {
        return self.a + self.b;
    }
}

impl Sum for R {
    fn total(R self) -> int {
        return self.a * self.b;
    }
}

fn mk(int n) -> P {
    return new P(n, 7);
}

fn mk_r(int n) -> R {
    return new R(n, 3);
}

fn ret(int n) -> Sum {
    return mk(n);
}

fn ret_r(int n) -> Sum {
    return mk_r(n);
}

fn total_of(Sum s) -> int {
    return s.total();
}

fn main() {
    let s: Sum = mk(2);
    assert!(s.total() == 9);
    let r: Sum = mk_r(2);
    assert!(r.total() == 6);
    assert!(ret(3).total() == 10);
    assert!(ret_r(3).total() == 9);
    assert!(total_of(mk(5)) == 12);
    let n = 4;
    let t: Sum = mk(n + 1);
    assert!(t.total() == 12);
}
