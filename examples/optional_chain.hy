// ?. optional field access + ?? fallback on a record (dict).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn show(Option o) -> int {
    return match o {
        Option::Some(n) => n,
        Option::None => 0,
    };
}

fn main() {
    let some = Option::Some({ v: 42 });
    let none = Option::None;
    write_all(stdout(), to_bytes(format("%i,", show(some?.v))));
    write_all(stdout(), to_bytes(format("%i", none?.v ?? 0)));
}
