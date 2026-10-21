// Output: go
use io::{stdout, write};
use string::{format, to_bytes};

fn early(int n) {
    if n == 0 {
        return;
    }
    let _ = write(stdout(), to_bytes("go"));
    return;
}

fn main() {
    early(0);
    early(1);
}
