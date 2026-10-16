// Expected: E0100 — `use math::nosuch` names nothing `math` defines, so a
// value use of `nosuch` is a name error, not a code-generation failure.
use math::nosuch;

fn main() {
    let f = nosuch;
}
