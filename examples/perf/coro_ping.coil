// CPU: coroutine resume/yield traffic.
use io::sync::print;
use string::format;

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
    let _ = print(format("%i", acc));
}
