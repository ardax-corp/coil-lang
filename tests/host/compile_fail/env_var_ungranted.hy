// @expected E0414 — `env::var` requires `--allow-env`; a program reaching it without the grant is rejected.
use env::var;

fn main() {
    let _ = var("HOME");
}
