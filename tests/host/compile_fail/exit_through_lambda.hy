// @expected E0407 — a call inside a lambda that `main` calls is reached too.
use env::exit;

fn main() {
    let code = 3;
    let stop = fn () use (code) => exit(code);
    stop();
}
