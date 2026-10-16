// Expected: E0132 — an assignment inside a loop body does not count after the loop.
fn main() {
    let s: string;
    let i = 0;
    while i < 1 {
        s = "x";
        i += 1;
    }
    let t = s;
}
