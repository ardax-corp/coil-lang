// Expected: E0406 — `env::exec` requires `--allow-exec`.
use env::{exec};

fn main() {
    let args: Vec<string> = Vec::new();
    let _ = exec("true", args);
}
