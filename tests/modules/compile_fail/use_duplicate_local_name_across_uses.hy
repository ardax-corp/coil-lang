// Expected: E0133 — two `use` statements binding `println` to different
// items clash, even when one of them is a builtin module (#1354).
use io::{println};
use math::{add as println};

fn main() {
    println("x");
}
