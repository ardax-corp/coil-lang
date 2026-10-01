// Expected: E0407 — `env::exit` requires `--allow-exit`.
use env::{exit};

fn main() {
    exit(0);
}
