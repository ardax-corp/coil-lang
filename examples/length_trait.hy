// examples/length_trait.hy — `len` via the Length typeclass.
//
// Output:
// 3
// 2
// 42

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

class Pair {
    pub a: int,
    pub b: int,
}

impl Length for Pair {
    fn len(Pair p) -> int {
        return 2;
    }
}

fn sized<T: Length>(T x) -> int {
    return len(x);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i\n", len("foo"))));
    write_all(stdout(), to_bytes(format("%i\n", len(new Pair(1, 2)))));
    write_all(stdout(), to_bytes(format("%i\n", sized("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"))));
}
