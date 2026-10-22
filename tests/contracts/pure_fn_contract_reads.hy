// Reading memory is no effect: a `pure fn` keeps `requires` / `ensures`
// clauses that index and measure its `Vec` argument.
// @output ok
use io::{stdout, write};
use string::to_bytes;

pure fn first(Vec<int> xs) -> int
    requires xs.len() > 0
    ensures result == xs[0]
{
    return xs[0];
}

fn main() {
    let v: Vec<int> = Vec::new();
    v.push(7);
    if !(first(v) == 7) {
        panic "assertion failed";
    }
    let _ = write(stdout(), to_bytes("ok"));
}
