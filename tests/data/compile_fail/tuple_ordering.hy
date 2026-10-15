// Expected: E0102 — tuples compare with `==` / `!=` only; `<` needs an `Ord` type.
fn main() {
    let b = (1, 2) < (1, 3);
}
