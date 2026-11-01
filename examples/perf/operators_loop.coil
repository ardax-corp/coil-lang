// CPU: Pow, BITAND, BITOR, LogNot in a tight loop.
use io::sync::print;
use string::format;

fn main() {
    let acc = 0;
    let i = 1;
    while i < 2000 {
        if !(i & 1) {
            acc += (i ** 2) & 255;
        } else {
            acc += (i | 3) & 127;
        }
        i = i + 1;
    }
    let _ = print(format("%i", acc));
}
