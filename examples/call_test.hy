// examples/call_test.hy — calling a function and discarding its result.
//
// `add(3, 4);` is an expression statement: the call runs and its value is
// dropped.
//
// Output: done

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    add(3, 4);
    write_all(stdout(), to_bytes("done"));
}
