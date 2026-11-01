// @expected E0119 — `==` on `Option<Box<Box<T>>>` needs `Eq<Box<T>>`, which needs `T`'s `Eq`; `T: Hash` has none (#1804).
#[derive(Eq)]
class Box<T> {
    pub v: T,
}

fn same<T: Hash>(Option<Box<Box<T>>> a, Option<Box<Box<T>>> b) -> bool {
    return a == b;
}

fn main() {
    let b = same(Some(new Box(new Box(1))), None);
}
