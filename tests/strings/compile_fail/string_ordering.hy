// Expected: E0102 — strings compare with `==` / `!=` only; `<` is not defined.
fn main() {
    let b = "a" < "b";
}
