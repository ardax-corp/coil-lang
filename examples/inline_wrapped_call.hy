// Calls whose bodies wrap another call: the tiny-call inliner starts copying
// such a body, reaches the callee's own local (which has no caller temp) and
// refuses, so a plain CALL is emitted. Regression anchor — a refused attempt
// must leave no partial body behind, or it runs ahead of the CALL and stores
// into caller slots.
//
// Output: 13

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn id(int n) -> int {
    return n;
}

fn wrap_add(int n) -> int {
    return 1 + id(n);
}

fn wrap_sub(int n) -> int {
    return 10 - id(n);
}

fn main() {
    // (1 + 5) + (10 - 3) == 13; a leaked body clobbers the left operand.
    write_all(stdout(), to_bytes(format("%i", wrap_add(5) + wrap_sub(3))));
}
