// Expected: E0300 — arrays have no `Show`; `%v` covers tuples and dicts structurally.
use string::format;

fn main() {
    let s = format("%v", [1, 2]);
}
