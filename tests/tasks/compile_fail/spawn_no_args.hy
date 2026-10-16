// Expected: E0119 — `spawn` needs a function to run.
use thread::spawn;

fn main() {
    let _ = spawn();
}
