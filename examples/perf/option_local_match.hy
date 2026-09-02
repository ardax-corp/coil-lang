// CPU: in-frame Option construct + match (no heap ObjEnum when it does not escape).
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn main() {
    let acc = 0;
    let i = 0;
    while i < 20000 {
        acc = acc + match Option::Some(i % 7) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        acc = acc + match Option::None {
            Option::Some(_) => 1,
            Option::None => 0,
        };
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
