//
// Homogeneous dicts iterate as (string, V) pairs. Print the values via
// tuple index. Insertion/table order is preserved by DictEntries.
//
// Output: 12

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let d = { a: 1, b: 2 };
    for p in d {
        write_all(stdout(), to_bytes(format("%i", p[1])));
    }
}
