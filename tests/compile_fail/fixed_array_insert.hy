// Expected: E0412 — `[T; N]` cannot grow (Q3).
fn main() {
    let xs: [int; 3] = [1, 2, 3];
    xs.insert(0, 0);
}
