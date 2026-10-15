// Rest-only generic with a typeclass constraint needs a call-site dict.
// Output: 1
use io::{stdout, write};
use string::{format, to_bytes};

fn show_all<T: Show>(T... xs) {
    write(stdout(), to_bytes(format("%v", xs[0])));
}

fn main() {
    show_all(1);
}
