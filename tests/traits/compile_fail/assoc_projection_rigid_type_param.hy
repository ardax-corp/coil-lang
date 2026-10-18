// Expected: E0119 — a type parameter `U` is not `C::Elem` (#1462).
trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem {}
}

impl Collect for Option<int> {
    type Elem = int;

    pub fn head(Option<int> xs) -> int {
        return xs ?? 0;
    }
}

fn bad<C: Collect, U>(C xs, U u) -> C::Elem {
    return u;
}

fn main() {
    let _ = bad(Option::Some(1), 2);
}
