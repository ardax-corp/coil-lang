// @expected E0119 — `D::Elem` is not `C::Elem` (#1462).
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

fn bad<C: Collect, D: Collect>(C a, D b, D::Elem e) -> C::Elem {
    return e;
}

fn main() {
    let _ = bad(Option::Some(1), Option::Some(2), 3);
}
