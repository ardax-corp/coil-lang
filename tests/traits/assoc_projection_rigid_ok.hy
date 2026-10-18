// `C::Elem` is rigid inside a generic body (#1462), but it still meets
// itself: a trait call on a `C`, a local or parameter annotated `C::Elem`,
// and a projection of another bounded parameter. Each call site still
// resolves it, and `C::Elem` and `D::Elem` resolve separately.
use tests::assert;

trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem {}
    fn count(C xs) -> int {}
}

class Words {
    pub w: string,
}

impl Collect for Option<int> {
    type Elem = int;

    pub fn head(Option<int> xs) -> int {
        return xs ?? 0;
    }

    pub fn count(Option<int> xs) -> int {
        return match xs {
            Option::Some(_) => 1,
            Option::None => 0,
        };
    }
}

impl Collect for Words {
    type Elem = string;

    pub fn head(Words w) -> string {
        return w.w;
    }

    pub fn count(Words w) -> int {
        return 1;
    }
}

fn take_head<C: Collect>(C xs) -> C::Elem {
    let e: C::Elem = head(xs);
    return e;
}

fn or_head<C: Collect>(C xs, C::Elem fallback) -> C::Elem {
    if count(xs) == 0 {
        return fallback;
    }
    return head(xs);
}

fn heads<C: Collect, D: Collect>(C a, D b) -> (C::Elem, D::Elem) {
    return (head(a), head(b));
}

test("a projection meets itself inside a generic body") {
    assert!(take_head(Option::Some(7)) + 1 == 8);
    assert!(take_head(new Words("x")) == "x");
    assert!(or_head(Option::None, 5) == 5);
    assert!(or_head(new Words("y"), "z") == "y");
    let (n, s) = heads(Option::Some(3), new Words("w"));
    assert!(n == 3);
    assert!(s == "w");
}
