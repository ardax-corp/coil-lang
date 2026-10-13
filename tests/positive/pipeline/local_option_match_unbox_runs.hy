// Output: 42,2
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let x = Option::Some(41);
    let y = match x {
        Option::Some(v) => v + 1,
        Option::None => 0,
    };
    let z = match Option::None {
        Option::Some(_) => 9,
        Option::None => 2,
    };
    write(stdout(), to_bytes(format("%i,%i", y, z)));
}
