use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let a = "hello";
    let b = "world";
    write_all(stdout(), to_bytes(format("%s", a + " " + b)));
    let s = format("%i-%s", 42, "x");
    write_all(stdout(), to_bytes(format("%s", s)));
}
