// @expected E0119 — `Box<T>` derives `Eq`, so `==` on it needs `T`'s `Eq`, and `T: Lt` has none (#1804); add `Eq` to `T`'s bound.
#[derive(Eq, Ord)]
class K {
    pub v: int,
}

#[derive(Eq)]
class Box<T> {
    pub v: T,
}

fn same<T: Lt>(Box<T> a, Box<T> b) -> bool {
    return a == b;
}

fn main() {
    let g = same;
    let b = g(new Box(new K(1)), new Box(new K(1)));
}
