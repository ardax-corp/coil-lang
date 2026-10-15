// Expected: E0112 — `let x = yield 1` outside a `gen fn` is rejected too.
fn main() {
    let x = yield 1;
}
