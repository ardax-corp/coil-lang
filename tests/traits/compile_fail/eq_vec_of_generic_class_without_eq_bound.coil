// @expected E0119 — `==` on `Vec<Box<T>>` compares each `Box<T>` with its derived `Eq`, which needs `T`'s `Eq`; `T: Lt` has none (#1804).
#[derive(Eq)]
class Box<T> {
    pub v: T,
}

fn same<T: Lt>(Vec<Box<T>> a, Vec<Box<T>> b) -> bool {
    return a == b;
}

fn main() {
    let b = same([new Box(1)], [new Box(1)]);
}
