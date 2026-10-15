// Expected: E0111 — a function with a non-unit return type must `return`
// on every path; falling off the end is rejected.
fn sign(int n) -> int {
    if n > 0 {
        return 1;
    }
}

fn main() {
    sign(1);
}
