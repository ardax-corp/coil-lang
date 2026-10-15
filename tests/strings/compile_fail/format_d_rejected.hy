// Expected: E0300 — `%d` is not a specifier; integers use `%i`.
use string::format;

fn main() {
    let s = format("%d", 1);
}
