// @expected E0119 — `Eq` is a superclass of `Ord` (#1716): `#[derive(Ord)]` needs an `Eq` instance (`#[derive(Eq, Ord)]`).
#[derive(Ord)]
class K {
    pub v: int,
}

fn main() {
    let b = new K(1) < new K(2);
}
