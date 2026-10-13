// Early-return diamond callees are tiny-inlined; both arms must still evaluate
// correctly at the call site.
// Output: -1,8
use io::{stdout, write};
use string::{format, to_bytes};

fn early(int n, int is_neg) -> int {
    if is_neg == 1 {
        return 0 - 1;
    }
    return n * 2;
}

fn main() {
    write(stdout(), to_bytes(format("%i,", early(4, 1))));
    write(stdout(), to_bytes(format("%i", early(4, 0))));
}
