// @expected E0119 — a tuple variant has no named fields; match it instead.
enum Tuple {
    Wrap(int, int),
}

fn main() {
    let p = Tuple::Wrap(1, 2);
    p.x;
}
