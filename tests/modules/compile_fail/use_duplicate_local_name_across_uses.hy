// @expected E0133 — two `use` statements binding `format` to different
// items clash, even when one of them is a builtin module (#1354).
use string::format;
use math::add as format;

fn main() {
    let s = format("%i", 1);
}
