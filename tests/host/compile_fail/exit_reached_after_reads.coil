// @expected E0407 — `env::exit` reached from `main` needs `--allow-exit`,
// even when the read calls before it are granted and the unreached `set_var`
// and `remove_file` helpers need nothing.
use env::{exit, set_var};
use io::open;
use io::fs::exists;
use io::fs::remove_file;

fn shell() {
    let _ = set_var("A", "b");
}

fn load() -> bool {
    let _ = open("cfg.toml", "r");
    let _ = exists("cfg.toml");
    return true;
}

fn main() {
    if load() {
        exit(0);
    }
}

fn cleanup() {
    let _ = remove_file("x");
}
