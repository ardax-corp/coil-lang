// Expected: compile failure — `[T; N]` cannot grow (Q3).
fn main() {
    let xs = [1, 2, 3];
    xs.reserve(8);
}
