// Expected: E0119 — under `T: Volume` only, `Area::size(x)` has no `Area` bound to use.
trait Area<T> {
    fn size(T s) -> int {}
}

trait Volume<T> {
    fn size(T s) -> int {}
}

fn f<T: Volume>(T x) -> int {
    return Area::size(x);
}

fn main() {}
