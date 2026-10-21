// `%v` formats a value of an open type parameter under a `Show` bound.
// Output: 10,20,
use io::{stdout, write};
use string::{format, to_bytes};

fn show_it<T: Show>(T x) {
    let _ = write(stdout(), to_bytes(format("%v,", x)));
}

fn main() {
    show_it(10);
    show_it(20);
}
