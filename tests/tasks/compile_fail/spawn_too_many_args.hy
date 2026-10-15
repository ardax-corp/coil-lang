// Expected: E0119 — `spawn` takes a function and at most one argument.
use thread::spawn;

fn work(int a, int b) -> int {
    return a + b;
}

fn main() {
    let _ = spawn(work, 1, 2);
}
