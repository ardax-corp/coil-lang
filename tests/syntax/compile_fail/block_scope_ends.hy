// Expected: E0100 — a `let` inside a block is not visible after the block.
fn main() {
    {
        let y = 2;
    }
    let z = y;
}
