// `len` of a string, tuple and dict bound to locals or returned from calls.
// @output 2\n3\n2\n
use io::{stdout, write};
use string::{format, to_bytes};

fn id_str(string s) -> string {
    return s;
}

fn main() {
    let t = (1, 2, 3);
    let d = { a: 1, b: 2 };
    let _ = write(stdout(), to_bytes(format("%i\n", id_str("ab").len())));
    let _ = write(stdout(), to_bytes(format("%i\n", t.len())));
    let _ = write(stdout(), to_bytes(format("%i\n", d.len())));
}
