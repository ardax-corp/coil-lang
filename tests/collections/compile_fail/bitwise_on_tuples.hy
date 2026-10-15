// Expected: E0119 — bitwise operators do not zip over aggregates.
fn main() {
    let _ = (1, 2) & (3, 4);
}
