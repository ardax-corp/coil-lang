// A generic call in a loop: its ground dictionary is built once into a
// static, not as a fresh tuple on every call.
use dict_lib::larger;
use string::{format, to_bytes};
use io::{stdout, write};

fn main() {
    let total = 0;
    for i in 0..1000 {
        total = total + larger(i, 500);
    }
    write(stdout(), to_bytes(format("%i\n", total)));
}
