// Expected: E0107 — a `let` typed `readonly Vec<int>` cannot be pushed to (#1558).
fn main() {
    let xs = [1, 2];
    let ys: readonly Vec<int> = xs;
    ys.push(3);
}
