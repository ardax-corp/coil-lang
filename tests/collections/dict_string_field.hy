// Dict fields holding strings and ints read back their values.
// Output: hi9
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let d = { name: "hi", n: 9 };
    write(stdout(), to_bytes(format("%s", d.name)));
    write(stdout(), to_bytes(format("%i", d.n)));
}
