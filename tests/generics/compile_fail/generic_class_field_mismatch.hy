// @expected E0102 — `Box<int>` holds an `int`; `new Box("s")` is a `Box<string>`.
class Box<T> {
    pub item: T,
}

fn main() {
    let b: Box<int> = new Box("s");
}
