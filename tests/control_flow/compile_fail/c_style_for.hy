// Expected: E0001 — C-style `for (init; cond; step)` is not Coil syntax;
// use `while` or `for x in range`.
fn main() {
    for (let i = 0; i < 5; i = i + 1) {
        i = i;
    }
}
