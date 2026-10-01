// Generic type aliases expand at typecheck time.
//
// Output: 7

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

type Pair<T> = (T, T);

fn main() {
    let p: Pair<int> = (3, 4);
    write_all(stdout(), to_bytes(format("%i", p[0] + p[1])));
}
