// Expected: E0119 — `!=` on `Box<T>` (deriving `Eq`) needs `T`'s `Eq`, and an unbounded `T` has none (#1804).
#[derive(Eq)]
class Box<T> {
    pub v: T,
}

fn differ<T>(Box<T> a, Box<T> b) -> bool {
    return a != b;
}

fn main() {
    let b = differ(new Box(1), new Box(2));
}
