// Phase P1: in-place dict mutation via `d.field = value` then re-read.
// Output: 99z
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let d = { foo: 1, name: "a" };
    d.foo = 99;
    d.name = "z";
    write(stdout(), to_bytes(format("%i", d.foo)));
    write(stdout(), to_bytes(format("%s", d.name)));
}
