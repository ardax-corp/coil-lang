// @expected E0119 — the format string must be a literal so its specifiers can be checked.
use string::format;

fn main() {
    let f = "%i";
    let s = format(f, 1);
}
