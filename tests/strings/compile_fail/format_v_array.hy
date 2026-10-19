// Expected: E0300 — `%v` shows an array through its elements' `Show`, and
// a function value has none (#1479).
use string::format;

fn one() -> int {
    return 1;
}

fn main() {
    let s = format("%v", [one]);
}
