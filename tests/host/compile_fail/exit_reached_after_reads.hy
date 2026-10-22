// @expected E0407 — `env::exit` reached from `main` needs `--allow-exit`,
// even when the read calls before it are granted and the unreached `exec`
// and `remove_file` helpers need nothing.
use env::{exec, exit};
use io::open;
use io::fs::exists;
use io::fs::remove_file;

fn shell() {
    let args: Vec<string> = Vec::new();
    let _ = exec("true", args);
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
