// @expected E0102 — ordering a class needs an `Ord` instance (`#[derive(Eq, Ord)]`).
class P {
    pub x: int,
}

fn main() {
    let b = new P(1) < new P(2);
}
