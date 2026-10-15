// Expected: E0111 — a non-unit function must return on every path.
fn sign(int a) -> int {
    if a > 0 {
        return 1;
    }
}

fn main() {
    let r = sign(1);
}
