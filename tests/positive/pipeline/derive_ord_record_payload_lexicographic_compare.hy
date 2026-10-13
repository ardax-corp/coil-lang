// Regression: `derive Ord` must emit Lt/Le/Gt/Ge + empty Ord (PR #14 layout)
// and lexicographic field compare must use strict `<` so equal prefixes fall
// through (a Leq-primary fold would short-circuit on equal leading fields).
// Output: true,true,false,true,false
use io::{stdout, write};
use string::{format, to_bytes};

#[derive(Ord)]
enum Pair {
    Pair { x: int, y: int },
}

fn main() {
    let a = Pair::Pair { x: 1, y: 2 };
    let b = Pair::Pair { x: 1, y: 3 };
    let c = Pair::Pair { x: 2, y: 0 };
    write(stdout(), to_bytes(format("%z,", a < b)));
    write(stdout(), to_bytes(format("%z,", a < c)));
    write(stdout(), to_bytes(format("%z,", b < a)));
    write(stdout(), to_bytes(format("%z,", a <= a)));
    write(stdout(), to_bytes(format("%z", a < a)));
}
