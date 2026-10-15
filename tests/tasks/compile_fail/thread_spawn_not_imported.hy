// Expected: E0101 — `spawn` lives in the `thread` module and needs `use thread::spawn`.
fn work() -> int {
    return 1;
}

fn main() {
    let t = spawn(work);
}
