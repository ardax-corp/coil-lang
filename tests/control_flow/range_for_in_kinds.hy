// `for` over int half-open and closed ranges, a range value, an empty
// decreasing range, a byte range and a float range (examples/range.hy).
// Output: 012340123561.02.03.0
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    for x in 0..5 {
        write_all(stdout(), to_bytes(format("%i", x)));
    }
    let r = 0..=3;
    for x in r {
        write_all(stdout(), to_bytes(format("%i", x)));
    }
    for x in 10..0 {
        write_all(stdout(), to_bytes(format("%i", x)));
    }
    let lo: byte = 5;
    let hi: byte = 6;
    for b in lo..=hi {
        write_all(stdout(), to_bytes(format("%i", b)));
    }
    for x in 1.0..4.0 {
        write_all(stdout(), to_bytes(format("%f", x)));
    }
}
