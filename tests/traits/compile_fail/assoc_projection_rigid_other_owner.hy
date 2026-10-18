// Expected: E0119 — a trait call on a `D` returns `D::Elem`, not `C::Elem` (#1462).
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

fn bad<C: Collect, D: Collect>(C a, D b) -> C::Elem {
    return head(b);
}

fn main() {
    let _ = bad(Option::Some(1), Option::Some(2));
}
