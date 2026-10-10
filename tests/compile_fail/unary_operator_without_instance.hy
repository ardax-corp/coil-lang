// Expected: E0102 — unary `-` / `~` on a class without `impl Neg` / `impl BitNot` (#812).
class V {
    pub x: int,
}

fn main() {
    let v = new V(1);
    let w = -v;
}
