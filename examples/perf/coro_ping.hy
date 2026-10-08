// CPU: coroutine resume/yield traffic.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

gen fn ping(int n) {
    let i = 0;
    while i < n {
        yield i;
        i = i + 1;
    }
}

fn main() {
    let h = ping(500);
    let acc = 0;
    let i = 0;
    while i < 500 {
        acc += resume h;
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
