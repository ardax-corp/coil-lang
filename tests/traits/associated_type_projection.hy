// `C::Elem` under `C: Collect` is an open type in a generic signature,
// pinned by the instance at each call site, in a return or a parameter type.
use tests::assert;

trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem {}
    fn count(C xs) -> int {}
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

fn take_head<C: Collect>(C xs) -> C::Elem {
    return head(xs);
}

fn head_if_any<C: Collect>(C xs, C::Elem fallback) -> C::Elem {
    if count(xs) == 0 {
        return fallback;
    }
    return head(xs);
}

test("a projection is pinned at the call site") {
    assert!(take_head(Option::Some(7)) + 1 == 8);
    assert!(take_head(Option::None) == 0);
}

test("a projection as a parameter type") {
    assert!(head_if_any(Option::None, 5) == 5);
    assert!(head_if_any(Option::Some(3), 5) == 3);
}
