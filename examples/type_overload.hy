// Same-arity overloads selected by argument type.
//
// @output i:7f:1.5s:hi

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

// Same-arity overloads selected by argument type.
fn show(int x) -> string {
    return format("i:%i", x);
}

fn show(float x) -> string {
    return format("f:%f", x);
}

fn show(string s) -> string {
    return format("s:%s", s);
}

fn main() {
    let _ = write_all(stdout(), to_bytes(show(7)));
    let _ = write_all(stdout(), to_bytes(show(1.5)));
    let _ = write_all(stdout(), to_bytes(show("hi")));
}
