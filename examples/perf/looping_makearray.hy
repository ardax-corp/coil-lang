// S2d: preheader MakeArray + computed-index store (maps + dense).
// 200000 stores; checksum 200000.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn bump() -> int {
    let arr = [0, 0, 0, 0];
    let i = 0;
    while i < 200000 {
        arr[i % 4] = arr[i % 4] + 1;
        i = i + 1;
    }
    return arr[0] + arr[1] + arr[2] + arr[3];
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", bump())));
}
