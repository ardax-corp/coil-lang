// ?. optional field access + ?? fallback on a record (dict).
//
// @output 42,0

use io::sync::print;
use string::format;

fn show(Option o) -> int {
    return match o {
        Option::Some(n) => n,
        Option::None => 0,
    };
}

fn main() {
    let some = Option::Some({ v: 42 });
    let none = Option::None;
    let _ = print(format("%i,", show(some?.v)));
    let _ = print(format("%i", none?.v ?? 0));
}
