// Local tuples read with constant indices: escape analysis turns them into
// slots, so the loop allocates nothing.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn churn(int n) -> int {
    let total = 0;
    let i = 0;
    while i < n {
        let t = (i, i * 3, i + 7);
        total = total + t[0] + t[1] - t[2];
        i = i + 1;
    }
    return total;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", churn(3000000))));
}
