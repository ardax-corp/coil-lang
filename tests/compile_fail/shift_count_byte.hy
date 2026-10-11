// Expected: E0102 — a shift count is always an `int`, whatever the type
// being shifted (#883).
fn main() {
    let a = 7 as byte;
    let b = a << (1 as byte);
}
