// Expected: compile failure — `env::exec` requires `--allow-exec`.
use env::{exec};

fn main() {
    let _ = exec("true", []);
}
