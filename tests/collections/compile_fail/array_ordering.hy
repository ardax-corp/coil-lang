// Expected: E0102 — arrays compare with `==` / `!=` only; `<` is not defined on them.
fn main() {
    let b = [1, 2] < [1, 3];
}
