// A multi-parameter trait with a `where` clause: `Convert<A, B>` with an
// `int` -> `int` identity instance.
//
// Output: 42

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

trait Convert<A, B> {
    fn cast(A x) -> B {}
}

impl Convert<int> for int {
    pub fn cast(int x) -> int {
        return x;
    }
}

fn apply_cast<A, B>(A x) -> B where Convert<A, B> {
    return cast(x);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", apply_cast(42))));
}
