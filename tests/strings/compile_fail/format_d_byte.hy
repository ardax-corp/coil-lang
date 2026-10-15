// Expected: E0300 — `%d` is not a specifier for a `byte` either; use `%i`.
use string::format;

fn main() {
    let b: byte = 7;
    let s = format("%d", b);
}
