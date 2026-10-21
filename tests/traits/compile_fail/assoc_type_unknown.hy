// Expected: E0119 — an impl may not define an associated type the trait lacks (`Extra`).
trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem {}
}

impl Collect for Option<int> {
    type Elem = int;
    type Extra = int;
    fn head(Option<int> xs) -> int {
        return 0;
    }
}

fn main() {
    head(Option::Some(1));
}
