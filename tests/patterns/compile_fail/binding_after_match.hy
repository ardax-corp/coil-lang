// @expected E0100 — a pattern binding does not outlive its match arm.
fn main() {
    let x = Option::Some(1);
    let r = match x {
        Option::Some(v) => 0,
        default => 1,
    };
    let y = v;
}
