// Expected: E0300 — `%z` takes a `bool`.
use string::format;

fn main() {
    let s = format("%z", 1);
}
