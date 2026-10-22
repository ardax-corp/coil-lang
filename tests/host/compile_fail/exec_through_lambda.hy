// @expected E0406 — a call inside a lambda that `main` calls is reached too.
use env::exec;

fn main() {
    let args: Vec<string> = Vec::new();
    let run = fn () use (args) => exec("true", args);
    let _ = run();
}
