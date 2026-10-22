// @expected E0119 — inside a generic body `C::Elem` is rigid, not `string` (#1462).
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

fn bad<C: Collect>(C xs) -> C::Elem {
    return "s";
}

fn main() {
    let _ = bad(Option::Some(1));
}
