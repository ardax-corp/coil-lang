// Expected: E0119 — unknown attribute names are rejected.
#[nope]
fn one() -> int {
    return 1;
}

fn main() {
    let r = one();
}
