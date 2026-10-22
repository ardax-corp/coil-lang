// @expected E0119 or E0301 — every `%` specifier needs an argument.
use string::format;

fn main() {
    let s = format("%i %i", 1);
}
