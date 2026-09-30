// CPU: enum construction + match dispatch in a loop.
// Variant names must not collide with builtin Option::None / Option::Some.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

enum Opt {
    Empty,
    Value(int),
}

fn payload(Opt o) -> int {
    return match o {
        Opt::Empty => 0,
        Opt::Value(x) => x,
    };
}

fn main() {
    let acc = 0;
    let i = 0;
    while i < 2000 {
        acc = acc + payload(Opt::Value((i % 7) + 1));
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
