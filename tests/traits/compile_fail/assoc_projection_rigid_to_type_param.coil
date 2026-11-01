// @expected E0119 — `C::Elem` is not a type parameter `U` (#1462).
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

fn bad<C: Collect, U>(C xs, C::Elem e) -> U {
    return e;
}

fn main() {
    let _: int = bad(Option::Some(1), 2);
}
