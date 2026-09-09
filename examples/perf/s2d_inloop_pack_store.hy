// In-loop MakeArray + Index + StoreIndex (computed index).
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        let xs = [0, 0, 0];
        xs[i % 3] = i;
        s = s + xs[i % 3];
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", pack(2000000))));
}
