// CPU: force sibling TailCall (even↔odd cycle). Tree recursion is not this shape.
// Parent emits CALL+RETURN; this rewrite emits TailCall. max_depth lets the
// parent compile the same source without overflowing analysis.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

#[max_depth(256)]
fn even(int n, int acc) -> int {
    if n == 0 {
        return acc;
    }
    return odd(n - 1, acc + n);
}

#[max_depth(256)]
fn odd(int n, int acc) -> int {
    if n == 0 {
        return acc;
    }
    return even(n - 1, acc + n);
}

fn main() {
    let s = 0;
    let i = 0;
    while i < 62500 {
        s = s + even(80, 0);
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", s)));
}
