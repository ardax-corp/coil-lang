// `return e;` inside an `async fn` now produces a real completion
// value (previously it was silently unified against `unit` and the
// typechecker rejected any non-unit `return`). The value returned by
// `return` propagates to the `resume` call that completes the
// coroutine, exactly like a yielded value.
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
