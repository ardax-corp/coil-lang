// Expected: compile failure — `[T; N]` cannot grow (Q3).
fn main() {
    let xs = [1, 2, 3];
    let _ = xs.pop();
}
