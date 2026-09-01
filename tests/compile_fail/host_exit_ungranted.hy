// Expected: compile failure — `env::exit` requires `--allow-exit`.
use env::{exit};

fn main() {
    exit(0);
}
