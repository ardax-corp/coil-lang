// Ground calls to another module's generic class methods: each gets a
// per-type clone compiled under that module, with no boxing or
// dictionaries.
use dict_lib::Tally;
use string::{format, to_bytes};
use io::{stdout, write};

fn main() {
    let t: Tally<int> = Tally::new();
    for i in 0..300 {
        t.add(i % 7);
    }
    write(stdout(), to_bytes(format("%i %i\n", t.count(3), t.count(9))));
}
