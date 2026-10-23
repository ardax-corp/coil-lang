// examples/call_test.hy — calling a function and discarding its result.
//
// `add(3, 4);` is an expression statement: the call runs and its value is
// dropped.
//
// @output done

use io::sync::print;

fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    add(3, 4);
    let _ = print("done");
}
