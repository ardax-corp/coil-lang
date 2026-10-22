// @expected E0119 — a trait call on a `C` returns `C::Elem`, not `int` (#1462).
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

fn bad<C: Collect>(C xs) -> int {
    return head(xs);
}

fn main() {
    let _ = bad(Option::Some(1));
}
