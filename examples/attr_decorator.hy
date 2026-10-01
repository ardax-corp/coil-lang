// Attribute macros (`examples/src/attr_demo.hy`) wrapping a function.
// Stacked macros apply outermost first: `log` wraps `measure` wraps the body.
//
// Output: enterdo_thinghi42

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};
use attr_demo::{log, measure};

#[log(message = "enter")]
#[measure(metric = "do_thing")]
fn do_thing(int x, string name) -> int {
    write_all(stdout(), to_bytes(format("%s", name)));
    return x;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", do_thing(42, "hi"))));
}
