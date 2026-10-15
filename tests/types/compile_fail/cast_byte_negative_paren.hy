// Expected: E0102 — `(-1) as byte` is a negative literal cast, rejected like `-1 as byte`.
fn main() {
    let x = (-1) as byte;
}
