// @expected E0119 — a positional attribute argument may not follow a named
// one (it would bind the first parameter again) (#1205).
use macro_probe::flags;

#[flags(label = "yo", times = 1, false)]
fn f() -> int {
    return 2;
}

fn main() {}
