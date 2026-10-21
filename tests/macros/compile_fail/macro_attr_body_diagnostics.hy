// Expected: E0102 — a type error in a function an attribute macro wraps is
// reported at the statement, not the whole item (#1822); so is the unused
// `Result` warning before it.
use io::stdout;
use io::sync::write_all;
use string::to_bytes;
use attr_demo::{log, measure};

#[log(message = "enter")]
#[measure(metric = "do_thing")]
fn do_thing(int x) -> int {
    write_all(stdout(), to_bytes("hi"));
    let y: int = "nope";
    return x + y;
}

fn main() {
    do_thing(1);
}
