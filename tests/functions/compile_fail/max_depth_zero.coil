// @expected E0119 — `#[max_depth(N)]` needs a positive N.
#[max_depth(0)]
fn walk(int n) -> int {
    if n == 0 {
        return 0;
    }
    return walk(n - 1);
}

fn main() {
    let r = walk(3);
}
