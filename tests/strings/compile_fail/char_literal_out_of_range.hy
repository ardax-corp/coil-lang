// Expected: E0102 — an int literal passed to `char` coerces to `byte` only in 0..=255 (#1175).
fn main() {
    let c = char(300);
}
