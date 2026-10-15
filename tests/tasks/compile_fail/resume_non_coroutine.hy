// Expected: E0102 — `resume` needs a coroutine handle, not an `int`.
fn plain() -> int {
    return 1;
}

fn main() {
    let h = plain();
    let _ = resume h;
}
