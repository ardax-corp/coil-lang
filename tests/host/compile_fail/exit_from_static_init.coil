// @expected E0407 — a static initializer runs at startup, so an `env::exit` it
// reaches needs `--allow-exit`.
use env::exit;

fn stop() -> int {
    exit(3);
    return 0;
}

static let STATUS: int = stop();

fn main() {}
