// Regression guard: two handles from the SAME parameterized
// `async fn`, interleaved, with `resume` used inline. Pre-fix, the
// same spurious trailing `POP` corrupted each coroutine's argument
// slot (`base`) on every resume after the first yield, producing
// wrong values once interleaving pushed other locals onto the
// shared stack.
// Output: 1,100,2,101,3,102
use io::{stdout, write};
use string::{format, to_bytes};

gen fn counter(int base) {
    yield base;
    yield base + 1;
    yield base + 2;
}

fn main() {
    let a = counter(1);
    let b = counter(100);

    write(stdout(), to_bytes(format("%i,", resume a)));
    write(stdout(), to_bytes(format("%i,", resume b)));
    write(stdout(), to_bytes(format("%i,", resume a)));
    write(stdout(), to_bytes(format("%i,", resume b)));
    write(stdout(), to_bytes(format("%i,", resume a)));
    write(stdout(), to_bytes(format("%i", resume b)));
}
