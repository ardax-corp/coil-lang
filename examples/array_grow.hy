use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let a = Vec::from([1, 2]);
    a.push(3);
    a.push(4);
    write_all(stdout(), to_bytes(format("%i", len(a))));
    write_all(stdout(), to_bytes(format("%i", a[0])));
    write_all(stdout(), to_bytes(format("%i", a[3])));
}
