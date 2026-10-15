// Expected: E0101 — `stdin` lives in the `io` module and needs `use io::stdin`.
fn main() {
    let s = stdin();
}
