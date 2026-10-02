// Expected: E0001 — C-style `for` was removed.
fn main() {
    for (let i = 0; i < 5; i = i + 1) {
        i = i;
    }
}
