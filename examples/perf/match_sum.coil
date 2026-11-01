// CPU: enum construction + match dispatch in a loop.
// Variant names must not collide with builtin Option::None / Option::Some.
use io::sync::print;
use string::format;

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
    let _ = print(format("%i", acc));
}
