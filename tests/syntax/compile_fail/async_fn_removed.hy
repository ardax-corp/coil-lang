// Expected: E0001 — `async fn` was removed; generator functions are `gen fn`.
async fn count() {
    yield 1;
}

fn main() {
    let h = count();
}
