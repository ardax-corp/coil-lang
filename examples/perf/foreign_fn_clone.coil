// A ground call to another module's generic function: it and the generic
// function it calls get per-type clones compiled under that module, with
// no boxing or dictionaries.
use dict_lib::largest;
use string::{format, to_bytes};
use io::{stdout, write};

fn main() {
    let xs: Vec<int> = Vec::new();
    for i in 0..300 {
        xs.push((i * 37) % 101);
    }
    let _ = write(stdout(), to_bytes(format("%i\n", largest(xs, 0))));
}
