// @expected E0204 — named constructor arguments must still give every field.
class P {
    pub x: int,
    pub y: int,
}

fn main() {
    let p = new P(y: 2);
}
