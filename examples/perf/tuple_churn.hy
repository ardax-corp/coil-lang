// Local tuples read with constant indices: escape analysis turns them into
// slots, so the loop allocates nothing.
use io::sync::print;
use string::format;

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
    let _ = print(format("%i", churn(3000000)));
}
