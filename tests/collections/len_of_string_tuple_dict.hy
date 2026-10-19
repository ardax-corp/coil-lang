// `len` of a string, tuple and dict bound to locals or returned from calls.
// Output: 2\n3\n2\n
use io::{stdout, write};
use string::{format, to_bytes};

fn id_str(string s) -> string {
    return s;
}

fn main() {
    let t = (1, 2, 3);
    let d = { a: 1, b: 2 };
    write(stdout(), to_bytes(format("%i\n", id_str("ab").len())));
    write(stdout(), to_bytes(format("%i\n", t.len())));
    write(stdout(), to_bytes(format("%i\n", d.len())));
}
