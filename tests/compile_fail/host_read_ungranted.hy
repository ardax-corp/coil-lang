// Expected: E0414 — `io::open` requires `--allow-read`, reached from `main` → `load`.
use io::open;

fn load() {
    let _ = open("cfg.toml", "r");
}

fn main() {
    load();
}
