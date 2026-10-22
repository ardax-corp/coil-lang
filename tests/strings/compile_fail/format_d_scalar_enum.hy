// @expected E0300 — `%d` is not a specifier, even for an int-backed enum
// (it used to compile and print `%d` literally, dropping the argument).
use string::format;

enum Level {
    Low = 0,
    High = 7,
}

fn main() {
    let s = format("[%d]", Level::High);
}
