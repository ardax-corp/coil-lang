// Phase P0: dict fields that hold heap objects (strings) must
// round-trip through GetField.
// Output: hi9
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let d = { name: "hi", n: 9 };
    write(stdout(), to_bytes(format("%s", d.name)));
    write(stdout(), to_bytes(format("%i", d.n)));
}
