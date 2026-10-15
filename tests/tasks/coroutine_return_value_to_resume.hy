// `return e;` in a `gen fn` is its completion value: the `resume` that
// finishes the coroutine evaluates to `e`, like a yielded value.
// Output: 1,2,42
use io::{stdout, write};
use string::{format, to_bytes};

gen fn counter() {
    yield 1;
    yield 2;
    return 42;
}

fn main() {
    let h = counter();
    write(stdout(), to_bytes(format("%i,", resume h))); // yield 1
    write(stdout(), to_bytes(format("%i,", resume h))); // yield 2
    write(stdout(), to_bytes(format("%i", resume h))); // return 42
}
