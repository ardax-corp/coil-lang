// Expected: E0119 — an impl must define each associated type of the trait (`Elem`).
trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem;
}

impl Collect for Option<int> {
    fn head(Option<int> xs) -> int {
        return 0;
    }
}

fn main() {
    head(Option::Some(1));
}
