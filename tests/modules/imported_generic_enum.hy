// A generic enum declared in another module type-checks there: its
// constructors and derived `Show` work when built and shown (#1356).
// Output: Tree::Leaf(3) 4
use io::{stdout, write};
use string::{format, to_bytes};
use generic_enum_provider::{leaf, full, Slot};

fn main() {
    let n = match full(4) {
        Slot::Full(v) => v,
        Slot::Free => 0,
    };
    let _ = write(stdout(), to_bytes(format("%v %i", leaf(3), n)));
}
