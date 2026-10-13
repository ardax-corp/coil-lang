// Regression guard: `resume h` used INLINE as a `print` argument
// (no intermediate `let` binding) must not corrupt the operand
// stack. Pre-fix, the bare `yield expr;` statement's spurious
// trailing `POP` (see `bare_yield_statement_does_not_emit_trailing_pop`
// in `compiler/src/lib.rs`) would pop whatever the resumer had
// already pushed for the in-progress `print` call (e.g. the format
// string), leading to a misaligned pointer dereference.
// Output: 0,1,2
use io::{stdout, write};
use string::{format, to_bytes};

gen fn counter() {
    yield 0;
    yield 1;
    yield 2;
}

fn main() {
    let h = counter();
    write(stdout(), to_bytes(format("%i,", resume h)));
    write(stdout(), to_bytes(format("%i,", resume h)));
    write(stdout(), to_bytes(format("%i", resume h)));
}
