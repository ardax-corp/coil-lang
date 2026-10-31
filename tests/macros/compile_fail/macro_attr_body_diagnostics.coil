// @expected E0102 — a type error in a function an attribute macro wraps is
// reported at the statement, not the whole item (#1822); so is the unused
// `Result` warning before it.
use derive_macros::add_after;

fn check(int x) -> Result<int, string> {
    return Ok(x);
}

#[add_after(1)]
#[add_after(by = 10)]
fn do_thing(int x) -> int {
    check(x);
    let y: int = "nope";
    return x + y;
}

fn main() {
    do_thing(1);
}
