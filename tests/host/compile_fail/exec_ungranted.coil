// @expected E0406 — starting a program needs `--allow-exec`, even through the `process` module.
use process::exec;

fn main() {
    let _ = exec("true", []);
}
