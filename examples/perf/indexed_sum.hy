// CPU: counted loop with `len(arr)` bound — ArrayLen should hoist once.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn sum(Vec<int> arr) -> int {
    let i = 0;
    let s = 0;
    while i < len(arr) {
        s = s + arr[i];
        i = i + 1;
    }
    return s;
}

fn main() {
    let v: Vec<int> = Vec::from([1, 2, 3, 4]);
    write_all(stdout(), to_bytes(format("%i", sum(v))));
}
