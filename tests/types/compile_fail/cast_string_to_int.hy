// Expected: E0102 — `as` casts primitive scalars only; a `string` is not cast to `int`.
fn main() {
    let x = "hi" as int;
}
