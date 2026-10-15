// Expected: E0107 — the const is constant again once the shadowing block ends.
fn main() {
    const x = 1;
    {
        let x = 2;
        x = 3;
    }
    x = 4;
}
