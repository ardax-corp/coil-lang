// Probe: boolean-variable guard (no compare to fuse into *Jmpf).
use io::sync::print;
use string::format;

fn count_until(bool stop, int n) -> int {
    let i = 0;
    let acc = 0;
    while i < n {
        if stop {
            break;
        }
        acc = acc + i;
        i = i + 1;
    }
    return acc;
}

fn main() {
    let _ = print(format("%i", count_until(false, 10)));
}
