// Expected: E0119 — range bounds need an `Ord` type; `string` is not ordered.
fn main() {
    let r = "a".."c";
}
