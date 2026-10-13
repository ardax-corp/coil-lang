// Output: 234
use io::{stdout, write};
use string::{format, to_bytes};

fn add1(Vec<int> xs) -> Vec<int> {
    return xs + 1;
}

fn main() {
    let a = add1([1, 2, 3]);
    write(stdout(), to_bytes(format("%i%i%i", a[0], a[1], a[2])));
}
