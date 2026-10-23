// CPU: iterative arithmetic + control flow (while, compound assign).
use io::sync::print;
use string::format;

fn main() {
    let acc = 0;
    let i = 0;
    while i < 2000 {
        acc = acc + i;
        i = i + 1;
    }
    let _ = print(format("%i", acc));
}
