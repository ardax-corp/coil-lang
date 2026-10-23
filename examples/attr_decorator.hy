// Attribute macros (`examples/src/attr_demo.hy`) wrapping a function.
// Stacked macros apply outermost first: `log` wraps `measure` wraps the body.
//
// @output enterdo_thinghi42

use io::sync::print;
use string::format;
use attr_demo::{log, measure};

#[log(message = "enter")]
#[measure(metric = "do_thing")]
fn do_thing(int x, string name) -> int {
    let _ = print(format("%s", name));
    return x;
}

fn main() {
    let _ = print(format("%i", do_thing(42, "hi")));
}
