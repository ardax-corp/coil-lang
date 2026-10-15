// Expected: E0204 — `new P(...)` takes one positional argument per field.
class P {
    pub x: int,
    pub y: int,
}

fn main() {
    let p = new P(1);
}
