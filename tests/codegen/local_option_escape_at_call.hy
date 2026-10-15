// Output: 7
use io::{stdout, write};
use string::{format, to_bytes};

fn take(Option<int> o) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => 0,
    };
}

fn main() {
    let x = Option::Some(7);
    write(stdout(), to_bytes(format("%i", take(x))));
}
