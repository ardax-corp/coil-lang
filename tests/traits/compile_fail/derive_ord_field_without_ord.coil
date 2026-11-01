// @expected E0102 — a derived `Ord` compares each field, and `Inner` has no `Lt`.
class Inner {
    pub v: int,
}

#[derive(Ord)]
class Outer {
    pub i: Inner,
}

fn main() {
    let b = new Outer(new Inner(1)) < new Outer(new Inner(2));
}
