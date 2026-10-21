// `#[derive(Ord)]` compares payload fields lexicographically in declaration
// order: equal leading fields fall through to the next field.
// Output: true,true,false,true,false
use io::{stdout, write};
use string::{format, to_bytes};

#[derive(Eq, Ord)]
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
