// @expected E0119 — `Lt for Box<T: Lt>` used at `Box<Plain>` needs `Lt<Plain>` (#1186).
class Plain {
    pub v: int,
}

class Box<T> {
    pub item: T,
}

impl Lt for Box<T: Lt> {
    fn lt(Box<T> a, Box<T> b) -> bool {
        return a.item < b.item;
    }
}

fn main() {
    let b = new Box(new Plain(1)) < new Box(new Plain(2));
}
