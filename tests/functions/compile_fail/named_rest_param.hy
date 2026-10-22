// @expected E0119 — rest parameters are positional-only.
fn head(int a, int... xs) -> int {
    return a + xs.len();
}

fn main() {
    let r = head(1, xs: 2);
}
