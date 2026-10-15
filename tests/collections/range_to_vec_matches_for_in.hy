// `.to_vec()` must yield the same sequence as `for` (shared LE/LEQ + step).
// Output: 0123|0123|7|1.02.03.0|1.02.03.0|0
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    for x in 0..=3 {
        write(stdout(), to_bytes(format("%i", x)));
    }
    write(stdout(), to_bytes("|"));
    let v = (0..=3).to_vec();
    let i = 0;
    while i < v.len() {
        write(stdout(), to_bytes(format("%i", v[i])));
        i = i + 1;
    }
    write(stdout(), to_bytes("|"));
    write(stdout(), to_bytes(format("%i", (7..=7).to_vec()[0])));
    write(stdout(), to_bytes("|"));
    for x in 1.0..=3.0 {
        write(stdout(), to_bytes(format("%f", x)));
    }
    write(stdout(), to_bytes("|"));
    let f = (1.0..=3.0).to_vec();
    let j = 0;
    while j < f.len() {
        write(stdout(), to_bytes(format("%f", f[j])));
        j = j + 1;
    }
    write(stdout(), to_bytes("|"));
    write(stdout(), to_bytes(format("%i", (4.0..1.0).to_vec().len())));
}
