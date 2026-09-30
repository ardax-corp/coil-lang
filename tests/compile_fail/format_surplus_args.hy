// Expected: compile failure — `{}` is not a format specifier, so both
// arguments are surplus (the VM would silently drop them).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let e = "boom";
    write_all(stdout(), to_bytes(format("Err([{}]) len={}", e, e.len())));
}
