// Expected output: 4242
//
// User trait dictionaries are consumed inside generic bodies and
// forwarded through nested generic calls.

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
trait Describable<T> {
    fn describe_val(T x) -> int;
}

impl Describable<int> {
    pub fn describe_val(int x) -> int {
        return x + 1;
    }
}

fn show<T: Describable>(T x) -> int {
    return x.describe_val();
}

fn show_ufcs<T: Describable>(T x) -> int {
    return describe_val(x);
}

fn outer<T: Describable>(T x) -> int {
    return show(x);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", show(41))));
    write_all(stdout(), to_bytes(format("%i", outer(41))));
}
