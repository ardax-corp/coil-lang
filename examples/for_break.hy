use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
fn main() {
    let sum = 0;
    let i = 0;
    while i < 10 {
        if i == 3 { i = i + 1; continue; }
        if i == 7 { break; }
        sum = sum + i;
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", sum)));
}
