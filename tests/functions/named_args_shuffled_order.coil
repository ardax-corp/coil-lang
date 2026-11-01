// Critical regression: shuffled named args must reorder to declaration
// order at runtime. Happy-path goldens only exercise source-order and
// positional-prefix forms; a missing codegen reorder would still typecheck
// here (string then int) but print the wrong values.
// @output Ada36Grace40
use io::{stdout, write};
use string::{format, to_bytes};

fn greet(string name, int age) {
    let _ = write(stdout(), to_bytes(format("%s", name)));
    let _ = write(stdout(), to_bytes(format("%i", age)));
}

fn main() {
    greet(age: 36, name: "Ada");
    greet(age: 40, name: "Grace");
}
