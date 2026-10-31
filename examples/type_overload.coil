// Same-arity overloads selected by argument type.
//
// @output i:7f:1.5s:hi

use io::sync::print;
use string::format;

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
    let _ = print(show(7));
    let _ = print(show(1.5));
    let _ = print(show("hi"));
}
