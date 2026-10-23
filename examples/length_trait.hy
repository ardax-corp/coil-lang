// examples/length_trait.hy — `len` via the Length typeclass.
//
// 3
// 2
// 42
//
// @output 3\n2\n42\n

use io::stdout;
use io::sync::println;
use io::sync::write_all;
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
    return x.len();
}

fn main() {
    let _ = println(format("%i", "foo".len()));
    let _ = println(format("%i", (new Pair(1, 2)).len()));
    let _ = write_all(
        stdout(),
        to_bytes(format("%i\n", sized("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"))),
    );
}
